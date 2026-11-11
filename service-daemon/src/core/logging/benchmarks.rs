//! Internal benchmark fixtures; no production API is exposed for benchmarking.
use super::{
    archive::ArchiveStore,
    file_config::{FileLogCompression, FileLogConfig},
    rotation::RotatingFileWriter,
};
use criterion::{BatchSize, Criterion, Throughput};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize},
    },
    time::{Duration, Instant, SystemTime},
};

struct Fixture {
    config: FileLogConfig,
}
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("sd-logging-benchmark-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&path).unwrap();
        let mut config = FileLogConfig::new(path.to_string_lossy(), "latest.log");
        // Measure processing work, excluding deliberate scheduling delays.
        config.archive.daily_start_jitter = Duration::ZERO;
        config.archive.monthly_start_jitter = Duration::ZERO;
        config.archive.monthly_volume_jitter = Duration::ZERO;
        Self { config }
    }
    fn path(&self, name: &str) -> PathBuf {
        PathBuf::from(&self.config.directory).join(name)
    }
    fn writer(&self) -> RotatingFileWriter {
        RotatingFileWriter::new(
            &self.config,
            Arc::new(Mutex::new(None)),
            Arc::new(AtomicUsize::new(0)),
            SystemTime::now(),
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.config.directory);
    }
}
fn corpus(bytes: usize) -> Vec<u8> {
    let mut result = Vec::new();
    let mut seed = 42_u64;
    while result.len() < bytes {
        result.extend_from_slice(b"{\"level\":\"DEBUG\",\"message\":\"");
        for _ in 0..200 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            result.push(b'a' + ((seed >> 32) % 26) as u8);
        }
        result.extend_from_slice(b"\"}\n");
    }
    result
}

pub(crate) fn run(criterion: &mut Criterion) {
    let record = corpus(1);
    let batch_lines = (1024_usize * 1024).div_ceil(record.len());
    let mut group = criterion.benchmark_group("file_logging/write");
    group.throughput(Throughput::Bytes((record.len() * batch_lines) as u64));
    for (name, size) in [
        ("time_only", None),
        ("time_and_size", Some(100 * 1024 * 1024)),
    ] {
        group.bench_function(name, |bench| {
            bench.iter_batched(
                || {
                    let mut fixture = Fixture::new();
                    fixture.config.max_file_size = size;
                    let writer = fixture.writer();
                    (fixture, writer)
                },
                |(fixture, mut writer)| {
                    for _ in 0..batch_lines {
                        writer.write_all(&record).unwrap();
                    }
                    (fixture, writer)
                },
                BatchSize::PerIteration,
            )
        });
    }
    group.finish();
    let mut group = criterion.benchmark_group("file_logging/rotation");
    group.bench_function("close_sync_rename_reopen", |bench| {
        bench.iter_batched(
            || {
                let mut fixture = Fixture::new();
                fixture.config.max_file_size = Some(record.len() as u64);
                let mut writer = fixture.writer();
                writer.write_all(&record).unwrap();
                (fixture, writer)
            },
            |(fixture, mut writer)| {
                writer.write_all(&record).unwrap();
                (fixture, writer)
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
    let corpus = corpus(1024 * 1024);
    let mut group = criterion.benchmark_group("file_logging/gzip");
    group.throughput(Throughput::Bytes(corpus.len() as u64));
    for level in [1, 6, 9] {
        group.bench_function(format!("level_{level}"), |bench| {
            bench.iter(|| {
                let mut encoder =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(level));
                encoder.write_all(&corpus).unwrap();
                std::hint::black_box(encoder.finish().unwrap());
            })
        });
    }
    group.finish();
}

#[test]
#[ignore = "opt-in disk/CPU measurement; cargo test -p service-daemon --release --features file-logging logging_performance -- --ignored --nocapture"]
fn logging_performance() {
    let fixture = Fixture::new();
    let bytes = corpus(1024 * 1024);
    let input_bytes = bytes.len() * 8;
    let mut expected = Sha256::new();
    let mut file =
        fs::File::create(fixture.path("latest.segment.2020-01-01-00-00-00.1.log")).unwrap();
    for _ in 0..8 {
        file.write_all(&bytes).unwrap();
        expected.update(&bytes);
    }
    file.sync_all().unwrap();
    drop(file);
    let start = Instant::now();
    let mut store = ArchiveStore::new(&fixture.config).unwrap();
    let cancel = AtomicBool::new(false);
    let mut steps = 0;
    while store.step(SystemTime::now(), &cancel).unwrap() {
        steps += 1;
    }
    let elapsed = start.elapsed();
    let volumes: Vec<_> = fs::read_dir(&fixture.config.directory)
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().contains(".monthly."))
        .collect();
    let compressed: u64 = volumes
        .iter()
        .map(|entry| entry.metadata().unwrap().len())
        .sum();
    let mut decoded = Sha256::new();
    let mut count = 0;
    for volume in &volumes {
        let mut reader = flate2::read::MultiGzDecoder::new(fs::File::open(volume.path()).unwrap());
        let mut buffer = [0; 64 * 1024];
        loop {
            let length = reader.read(&mut buffer).unwrap();
            if length == 0 {
                break;
            }
            decoded.update(&buffer[..length]);
            count += length;
        }
    }
    assert_eq!(count, input_bytes);
    assert_eq!(decoded.finalize(), expected.finalize());
    eprintln!(
        "file_logging pipeline: input_bytes={}, compressed_bytes={compressed}, elapsed_ms={}, scheduler_steps={steps}, volumes={}",
        input_bytes,
        elapsed.as_millis(),
        volumes.len()
    );
    #[cfg(target_os = "linux")]
    if let Ok(status) = fs::read_to_string("/proc/self/status") {
        for line in status
            .lines()
            .filter(|line| line.starts_with("VmHWM:") || line.starts_with("VmRSS:"))
        {
            eprintln!("{line}");
        }
    }
    for level in [1, 6, 9] {
        let start = Instant::now();
        let mut encoder =
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(level));
        for _ in 0..8 {
            encoder.write_all(&bytes).unwrap();
        }
        let output = encoder.finish().unwrap();
        eprintln!(
            "gzip level={level}: input_bytes={}, compressed_bytes={}, elapsed_ms={}",
            input_bytes,
            output.len(),
            start.elapsed().as_millis()
        );
    }
}

fn write_100mib_fragment() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.config.compression = FileLogCompression::None;
    fixture.config.rotation_jitter = 0.0;
    let mut line = b"{\"message\":\"".to_vec();
    line.resize(1021, b'x');
    line.extend_from_slice(b"\"}\n");
    let mut writer = fixture.writer();
    for _ in 0..(100 * 1024) {
        writer.write_all(&line).unwrap();
    }
    writer.write_all(b"next\n").unwrap();
    drop(writer);
    let fragment = fs::read_dir(&fixture.config.directory)
        .unwrap()
        .flatten()
        .find(|entry| entry.file_name().to_string_lossy().contains(".segment."))
        .unwrap();
    assert_eq!(fragment.metadata().unwrap().len(), 100 * 1024 * 1024);
    fs::remove_file(fragment.path()).unwrap();
    fixture
}

#[test]
#[ignore = "opt-in actual 100MiB boundary fixture"]
fn logging_scale_100mib() {
    drop(write_100mib_fragment());
}

#[test]
#[ignore = "large opt-in fixture: writes and recompresses about 2GiB; see release-validation.md"]
fn logging_scale_100mib_and_1gib() {
    let fixture = write_100mib_fragment();
    let path = fixture.path("latest.daily.2020-01-01.1.log.gz");
    let block = corpus(1024 * 1024);
    let mut encoder = flate2::write::GzEncoder::new(
        fs::File::create(&path).unwrap(),
        flate2::Compression::new(6),
    );
    for _ in 0..2048 {
        encoder.write_all(&block).unwrap();
    }
    encoder.finish().unwrap().sync_all().unwrap();
    let mut store = ArchiveStore::new(&fixture.config).unwrap();
    let cancel = AtomicBool::new(false);
    while store.step(SystemTime::now(), &cancel).unwrap() {}
    let volumes: Vec<_> = fs::read_dir(&fixture.config.directory)
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().contains(".monthly."))
        .collect();
    assert!(volumes.len() >= 2);
    let expected = block.len() as u64 * 2048;
    let mut decoded = 0;
    for entry in volumes {
        let mut decoder = flate2::read::MultiGzDecoder::new(fs::File::open(entry.path()).unwrap());
        decoded += std::io::copy(&mut decoder, &mut std::io::sink()).unwrap();
    }
    assert_eq!(decoded, expected);
}

/// Alternative benchmark entry point for runners that invoke ignored Rust tests.
#[test]
#[ignore = "Criterion disk/CPU benchmarks; prefer cargo bench --features file-logging --bench framework file_logging"]
fn logging_criterion_benchmarks() {
    let mut criterion = Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(2));
    run(&mut criterion);
    criterion.final_summary();
}
