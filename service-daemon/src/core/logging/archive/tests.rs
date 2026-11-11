use super::*;

struct Fixture {
    config: FileLogConfig,
}
impl Fixture {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("sd-archive-v2-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&directory).unwrap();
        let mut config = FileLogConfig::new(directory.to_string_lossy(), "latest.log");
        config.archive.daily_start_jitter = Duration::ZERO;
        config.archive.monthly_start_jitter = Duration::ZERO;
        config.archive.monthly_volume_jitter = Duration::ZERO;
        Self { config }
    }
    fn path(&self, name: &str) -> PathBuf {
        Path::new(&self.config.directory).join(name)
    }
    fn gzip(&self, name: &str, bytes: &[u8]) {
        let mut encoder =
            GzEncoder::new(File::create(self.path(name)).unwrap(), Compression::fast());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap().sync_all().unwrap();
    }
    fn store(&self) -> ArchiveStore {
        ArchiveStore::new(&self.config).unwrap()
    }
    fn files(&self, marker: &str) -> Vec<PathBuf> {
        let mut files: Vec<_> = fs::read_dir(&self.config.directory)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.file_name().unwrap().to_string_lossy().contains(marker))
            .collect();
        files.sort();
        files
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.config.directory);
    }
}
fn now(day: &str) -> SystemTime {
    NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
        .and_utc()
        .into()
}
fn read(path: &Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    MultiGzDecoder::new(File::open(path).unwrap())
        .read_to_end(&mut bytes)
        .unwrap();
    bytes
}
fn drain(store: &mut ArchiveStore, clock: SystemTime) -> Vec<String> {
    let cancel = AtomicBool::new(false);
    let mut errors = Vec::new();
    for _ in 0..20000 {
        match store.step(clock, &cancel) {
            Ok(false) => return errors,
            Ok(true) => {}
            Err(error) => errors.push(error.to_string()),
        }
    }
    panic!("archive scheduler did not drain: {errors:?}");
}
fn corpus(lines: usize) -> Vec<u8> {
    let mut seed = 12345_u64;
    let mut data = Vec::new();
    for index in 0..lines {
        data.extend_from_slice(format!("{{\"index\":{index},\"message\":\"").as_bytes());
        for _ in 0..100 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            data.push(b'a' + ((seed >> 32) % 26) as u8);
        }
        data.extend_from_slice(b"\"}\n");
    }
    data
}

#[test]
fn daily_start_jitter_defers_backlog_without_delaying_fragment_compression() {
    let mut fixture = Fixture::new();
    fixture.config.archive.daily_start_jitter = Duration::from_secs(1800);
    let name = "latest.segment.2020-01-01-01-00-00.1.log";
    fs::write(fixture.path(name), b"queued\n").unwrap();
    assert!(drain(&mut fixture.store(), now("2020-01-03")).is_empty());
    assert!(fixture.path(&format!("{name}.gz")).exists());
    assert!(fixture.files(".daily.").is_empty());
}

#[test]
fn monthly_start_jitter_defers_ready_historical_daily_archives() {
    let mut fixture = Fixture::new();
    fixture.config.archive.monthly_start_jitter = Duration::from_secs(1800);
    let name = "latest.daily.2020-01-01.1.log.gz";
    fixture.gzip(name, b"queued\n");
    assert!(drain(&mut fixture.store(), now("2020-02-03")).is_empty());
    assert!(fixture.path(name).exists());
    assert!(fixture.files(".monthly.").is_empty());
}

#[test]
fn monthly_volume_jitter_waits_after_publication_and_preserves_cross_volume_source() {
    let mut fixture = Fixture::new();
    fixture.config.archive.monthly_volume_size = 4096;
    fixture.config.archive.monthly_volume_jitter = Duration::from_secs(1800);
    let name = "latest.daily.2020-01-01.1.log.gz";
    let input = corpus(3000);
    fixture.gzip(name, &input);
    let mut store = fixture.store();
    assert!(drain(&mut store, now("2020-02-03")).is_empty());
    assert_eq!(fixture.files(".monthly.").len(), 1);
    assert!(fixture.path(name).exists());
    let fragment = "latest.segment.2020-02-03-01-00-00.1.log";
    fs::write(fixture.path(fragment), b"new\n").unwrap();
    store.request();
    assert!(drain(&mut store, now("2020-02-03")).is_empty());
    assert_eq!(fixture.files(".monthly.").len(), 1);
    assert_eq!(read(&fixture.path(&format!("{fragment}.gz"))), b"new\n");
    let first = fixture.files(".monthly.")[0].clone();
    let first_bytes = fs::read(&first).unwrap();
    let (path, manifest) = manifests(&store)
        .into_iter()
        .find(|(_, manifest)| manifest.stage == Stage::Monthly)
        .unwrap();
    let deadline = manifest.wait.unwrap().until;
    drop(store);
    let mut restarted = fixture.store();
    assert!(drain(&mut restarted, now("2020-02-03")).is_empty());
    assert_eq!(fixture.files(".monthly.").len(), 1);
    let mut manifest: Manifest = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(manifest.wait.as_ref().unwrap().until, deadline);
    // Advance the persisted clock and disable further waits for the remaining volumes.
    manifest.wait.as_mut().unwrap().until = SystemTime::UNIX_EPOCH;
    manifest.volume_jitter = Duration::ZERO;
    save_json(&path, &manifest).unwrap();
    drop(restarted);
    assert!(drain(&mut fixture.store(), now("2020-02-03")).is_empty());
    assert_eq!(fs::read(first).unwrap(), first_bytes);
    assert_eq!(
        fixture
            .files(".monthly.")
            .iter()
            .flat_map(|path| read(path))
            .collect::<Vec<_>>(),
        input
    );
    assert!(!fixture.path(name).exists());
}

fn manifests(store: &ArchiveStore) -> Vec<(PathBuf, Manifest)> {
    fs::read_dir(&store.transactions)
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .map(|entry| {
            let path = entry.path();
            let manifest = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            (path, manifest)
        })
        .collect()
}

#[test]
fn archive_jitter_deadlines_are_independent_bounded_and_survive_restart() {
    let mut fixture = Fixture::new();
    fixture.config.archive.daily_start_jitter = Duration::from_secs(1800);
    for day in 1..=8 {
        fixture.gzip(&format!("latest.2020-01-{day:02}.log.gz"), b"queued\n");
    }
    let before = SystemTime::now();
    let mut store = fixture.store();
    assert!(drain(&mut store, now("2020-01-15")).is_empty());
    let deadlines: Vec<_> = manifests(&store)
        .into_iter()
        .map(|(path, manifest)| {
            let until = manifest.wait.unwrap().until;
            assert!(until >= before);
            assert!(until <= SystemTime::now() + Duration::from_secs(1800));
            (path, until)
        })
        .collect();
    assert_eq!(deadlines.len(), 8);
    assert!(deadlines.iter().any(|(_, until)| *until != deadlines[0].1));
    drop(store);
    let mut restarted = fixture.store();
    assert!(drain(&mut restarted, now("2020-01-15")).is_empty());
    assert!(fixture.files(".daily.").is_empty());
    for (path, until) in deadlines {
        let manifest: Manifest = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(manifest.wait.unwrap().until, until);
    }
}

#[test]
fn waiting_archives_do_not_block_ready_dates_or_months_and_expire_without_resampling() {
    for (stage, first, unrelated, date, clock, marker) in [
        (
            Stage::Daily,
            "latest.2020-01-01.log.gz",
            "latest.2020-01-02.log.gz",
            "2020-01-01",
            now("2020-01-05"),
            ".daily.",
        ),
        (
            Stage::Monthly,
            "latest.daily.2020-01-01.1.log.gz",
            "latest.daily.2020-02-01.1.log.gz",
            "2020-01",
            now("2020-03-03"),
            ".monthly.",
        ),
    ] {
        let fixture = Fixture::new();
        fixture.gzip(first, b"waiting\n");
        fixture.gzip(unrelated, b"ready\n");
        let store = fixture.store();
        let path = store
            .create(stage, date.into(), &[store.parse(first).unwrap()])
            .unwrap();
        let mut manifest: Manifest = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest.wait = Some(ArchiveWait {
            until: SystemTime::now() + Duration::from_secs(1800),
            limit: Duration::from_secs(1800),
        });
        save_json(&path, &manifest).unwrap();
        let mut store = fixture.store();
        assert!(drain(&mut store, clock).is_empty());
        assert_eq!(fixture.files(marker).len(), 1);
        assert_eq!(read(&fixture.files(marker)[0]), b"ready\n");
        assert!(fixture.path(first).exists());
        manifest.wait.as_mut().unwrap().until = SystemTime::UNIX_EPOCH;
        save_json(&path, &manifest).unwrap();
        drop(store);
        assert!(drain(&mut fixture.store(), clock).is_empty());
        assert_eq!(fixture.files(marker).len(), 2);
        assert!(!fixture.path(first).exists());
    }
}

#[test]
fn pre_jitter_v2_transactions_adopt_persisted_start_delays_once() {
    let mut fixture = Fixture::new();
    fixture.config.archive.daily_start_jitter = Duration::from_secs(1800);
    let name = "latest.2020-01-01.log.gz";
    fixture.gzip(name, b"legacy\n");
    let store = fixture.store();
    let path = store
        .create(
            Stage::Daily,
            "2020-01-01".into(),
            &[store.parse(name).unwrap()],
        )
        .unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value.as_object_mut().unwrap().remove("wait");
    value.as_object_mut().unwrap().remove("volume_jitter");
    save_json(&path, &value).unwrap();
    assert!(drain(&mut fixture.store(), now("2020-01-05")).is_empty());
    let manifest: Manifest = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(manifest.wait.is_some());
    assert!(fixture.path(name).exists());
    assert!(fixture.files(".daily.").is_empty());
}

#[test]
fn archive_wait_bounds_backward_clock_changes_on_restart_and_accepts_zero() {
    let limit = Duration::from_secs(1800);
    let wait = ArchiveWait {
        until: SystemTime::now() + Duration::from_secs(86400),
        limit,
    };
    let before = Instant::now();
    let deadline = wait.deadline().unwrap();
    assert!(deadline >= before + limit);
    assert!(deadline <= Instant::now() + limit);
    let wait = ArchiveWait::sample(Duration::ZERO).unwrap();
    assert!(wait.deadline().unwrap() <= Instant::now());
}

#[test]
fn parked_month_yields_to_another_month_and_resumes_its_reader_in_memory() {
    let mut fixture = Fixture::new();
    fixture.config.archive.monthly_volume_size = 4096;
    fixture.config.archive.monthly_volume_jitter = Duration::from_secs(1800);
    let input = corpus(3000);
    fixture.gzip("latest.daily.2020-01-01.1.log.gz", &input);
    fixture.gzip("latest.daily.2020-02-01.1.log.gz", b"other month\n");
    let mut store = fixture.store();
    let clock = now("2020-03-03");
    assert!(drain(&mut store, clock).is_empty());
    assert_eq!(
        read(&fixture.files(".monthly.2020-02.")[0]),
        b"other month\n"
    );
    assert_eq!(store.paused.len(), 1);
    assert!(store.paused[0].reader.is_some());
    assert!(store.paused[0].encoder.is_none());
    assert!(!store.step(clock, &AtomicBool::new(true)).unwrap());
    store.paused[0].wake = Instant::now();
    store.paused[0].manifest.volume_jitter = Duration::ZERO;
    assert!(drain(&mut store, clock).is_empty());
    assert!(store.paused.is_empty());
    assert_eq!(
        fixture
            .files(".monthly.2020-01.")
            .iter()
            .flat_map(|path| read(path))
            .collect::<Vec<_>>(),
        input
    );
}

#[test]
fn closed_same_period_fragment_is_compressed_immediately_at_fast_level() {
    let fixture = Fixture::new();
    let name = "latest.segment.2020-01-01-01-00-00.1.log";
    fs::write(fixture.path(name), b"one\n").unwrap();
    fs::write(fixture.path("latest.log"), b"active\n").unwrap();
    let mut store = fixture.store();
    assert!(drain(&mut store, now("2020-01-01")).is_empty());
    let gzip = fixture.path(&format!("{name}.gz"));
    assert_eq!(read(&gzip), b"one\n");
    assert_eq!(fs::read(gzip).unwrap()[8], 4); // gzip XFL: fastest algorithm
    assert!(!fixture.path(name).exists());
    assert_eq!(fs::read(fixture.path("latest.log")).unwrap(), b"active\n");
}
#[test]
fn one_fragment_is_reencoded_to_medium_daily_archive_after_grace() {
    let fixture = Fixture::new();
    fixture.gzip("latest.segment.2020-01-01-01-00-00.1.log.gz", b"one\n");
    let mut store = fixture.store();
    assert!(drain(&mut store, now("2020-01-02")).is_empty());
    assert!(fixture.files(".daily.").is_empty());
    store.request();
    assert!(drain(&mut store, now("2020-01-03")).is_empty());
    let daily = fixture.files(".daily.");
    assert_eq!(daily.len(), 1);
    assert_eq!(read(&daily[0]), b"one\n");
    assert_eq!(fs::read(&daily[0]).unwrap()[8], 0);
}
#[test]
fn numeric_sequence_order_and_legacy_naming_are_independent_of_current_policy() {
    let fixture = Fixture::new();
    for (sequence, body) in [(10, b"ten\n".as_slice()), (2, b"two\n"), (1, b"one\n")] {
        fixture.gzip(&format!("latest.2020-01-01-01.{sequence}.log.gz"), body);
    }
    fixture.gzip("latest.log.2020-01-01-02.gz", b"legacy\n");
    let mut store = fixture.store();
    assert!(drain(&mut store, now("2020-01-03")).is_empty());
    assert_eq!(
        read(&fixture.files(".daily.")[0]),
        b"one\ntwo\nten\nlegacy\n"
    );
}
#[test]
fn malformed_and_unrelated_names_are_not_candidates() {
    let fixture = Fixture::new();
    let store = fixture.store();
    for name in [
        "latest.log",
        "other.2020-01-01.log",
        "latest.2020-01-01.red.log.gz",
        "latest.monthly.2020-01.part0001.log.gz",
        "latest.segment.2020-01-01-01-00-00.0.log.gz",
    ] {
        assert!(store.parse(name).is_none(), "{name}");
    }
}
#[test]
fn daily_archive_waits_until_the_active_starting_date_is_closed() {
    use super::super::rotation::RotatingFileWriter;
    use parking_lot::Mutex;
    use std::sync::{Arc, atomic::AtomicUsize};
    let fixture = Fixture::new();
    let mut writer = RotatingFileWriter::new(
        &fixture.config,
        Arc::new(Mutex::new(None)),
        Arc::new(AtomicUsize::new(0)),
        now("2020-01-01"),
    )
    .unwrap();
    writer.write_all(b"active\n").unwrap();
    fixture.gzip("latest.segment.2020-01-01-01-00-00.1.log.gz", b"closed\n");
    let mut store = fixture.store();
    assert!(drain(&mut store, now("2020-01-05")).is_empty());
    assert!(fixture.files(".daily.").is_empty());
}
#[test]
fn monthly_volumes_preserve_lines_content_and_high_compression() {
    let mut fixture = Fixture::new();
    fixture.config.archive.monthly_volume_size = 4096;
    let input = corpus(3000);
    fixture.gzip("latest.daily.2020-01-01.1.log.gz", &input);
    let mut store = fixture.store();
    assert!(drain(&mut store, now("2020-02-03")).is_empty());
    let volumes = fixture.files(".monthly.");
    assert!(volumes.len() > 1);
    let mut result = Vec::new();
    for volume in volumes {
        let bytes = read(&volume);
        assert!(bytes.ends_with(b"\n"));
        assert!(!bytes.is_empty());
        assert_eq!(fs::read(&volume).unwrap()[8], 2);
        result.extend(bytes);
    }
    assert_eq!(result, input);
    assert!(fixture.files(".daily.").is_empty());
}
#[test]
fn restart_preserves_published_volumes_and_keeps_cross_volume_source_until_covered() {
    let mut fixture = Fixture::new();
    fixture.config.archive.monthly_volume_size = 4096;
    let input = corpus(3000);
    let source = "latest.daily.2020-01-01.1.log.gz";
    fixture.gzip(source, &input);
    let clock = now("2020-02-03");
    let mut store = fixture.store();
    let cancel = AtomicBool::new(false);
    while fixture.files(".monthly.").is_empty() {
        store.step(clock, &cancel).unwrap();
    }
    let first = fixture.files(".monthly.")[0].clone();
    let original = fs::read(&first).unwrap();
    assert!(fixture.path(source).exists());
    drop(store);
    let mut store = fixture.store();
    assert!(drain(&mut store, clock).is_empty());
    assert_eq!(fs::read(first).unwrap(), original);
    let result: Vec<_> = fixture
        .files(".monthly.")
        .iter()
        .flat_map(|path| read(path))
        .collect();
    assert_eq!(result, input);
    assert!(!fixture.path(source).exists());
}
#[test]
fn new_fragment_preempts_an_in_progress_monthly_encoder() {
    let mut fixture = Fixture::new();
    fixture.config.archive.monthly_volume_size = 1024 * 1024;
    fixture.gzip("latest.daily.2020-01-01.1.log.gz", &corpus(3000));
    let mut store = fixture.store();
    let clock = now("2020-02-03");
    let cancel = AtomicBool::new(false);
    while store.jobs[2]
        .as_ref()
        .is_none_or(|job| job.cursor.offset == 0)
    {
        store.step(clock, &cancel).unwrap();
    }
    let offset = store.jobs[2].as_ref().unwrap().cursor.offset;
    let name = "latest.segment.2020-02-03-01-00-00.1.log";
    fs::write(fixture.path(name), b"urgent\n").unwrap();
    store.request();
    while !fixture.path(&format!("{name}.gz")).exists() {
        store.step(clock, &cancel).unwrap();
        assert_eq!(store.jobs[2].as_ref().unwrap().cursor.offset, offset);
    }
}
#[test]
fn corrupt_day_is_retained_while_an_independent_day_succeeds() {
    let fixture = Fixture::new();
    fs::write(fixture.path("latest.2020-01-01-01.log.gz"), b"not gzip").unwrap();
    fixture.gzip("latest.2020-01-02-01.log.gz", b"good\n");
    let mut store = fixture.store();
    let errors = drain(&mut store, now("2020-01-05"));
    assert!(!errors.is_empty());
    assert!(fixture.path("latest.2020-01-01-01.log.gz").exists());
    assert_eq!(read(&fixture.files(".daily.")[0]), b"good\n");
}
#[test]
fn cancellation_preserves_source_and_publishes_no_partial_output() {
    let fixture = Fixture::new();
    fixture.gzip("latest.daily.2020-01-01.1.log.gz", &corpus(3000));
    let mut store = fixture.store();
    assert!(
        !store
            .step(now("2020-02-03"), &AtomicBool::new(true))
            .unwrap()
    );
    assert!(fixture.files(".monthly.").is_empty());
    assert!(fixture.path("latest.daily.2020-01-01.1.log.gz").exists());
}
#[test]
fn existing_gzip_is_never_overwritten_by_an_unproven_plain_source() {
    let fixture = Fixture::new();
    let raw = "latest.2020-01-01.log";
    fs::write(fixture.path(raw), b"source\n").unwrap();
    fixture.gzip(&format!("{raw}.gz"), b"different\n");
    let original = fs::read(fixture.path(&format!("{raw}.gz"))).unwrap();
    let mut store = fixture.store();
    assert!(!drain(&mut store, now("2020-01-01")).is_empty());
    assert_eq!(fs::read(fixture.path(raw)).unwrap(), b"source\n");
    assert_eq!(
        fs::read(fixture.path(&format!("{raw}.gz"))).unwrap(),
        original
    );
}
#[test]
fn late_day_is_appended_as_new_volume_without_rewriting_old_month() {
    let fixture = Fixture::new();
    fixture.gzip("latest.daily.2020-01-01.1.log.gz", b"first\n");
    let mut store = fixture.store();
    let clock = now("2020-02-03");
    assert!(drain(&mut store, clock).is_empty());
    let first = fixture.files(".monthly.")[0].clone();
    let original = fs::read(&first).unwrap();
    fixture.gzip("latest.2020-01-02.log.gz", b"late\n");
    store.request();
    assert!(drain(&mut store, clock).is_empty());
    assert_eq!(fixture.files(".monthly.").len(), 2);
    assert_eq!(fs::read(first).unwrap(), original);
    assert_eq!(
        fixture
            .files(".monthly.")
            .iter()
            .flat_map(|path| read(path))
            .collect::<Vec<_>>(),
        b"first\nlate\n"
    );
}
#[test]
fn corrupt_published_volume_blocks_source_cleanup_on_restart() {
    let mut fixture = Fixture::new();
    fixture.config.archive.monthly_volume_size = 4096;
    fixture.gzip("latest.daily.2020-01-01.1.log.gz", &corpus(3000));
    let mut store = fixture.store();
    let clock = now("2020-02-03");
    while fixture.files(".monthly.").is_empty() {
        store.step(clock, &AtomicBool::new(false)).unwrap();
    }
    fs::write(&fixture.files(".monthly.")[0], b"corrupt").unwrap();
    drop(store);
    let mut store = fixture.store();
    assert!(!drain(&mut store, clock).is_empty());
    assert!(fixture.path("latest.daily.2020-01-01.1.log.gz").exists());
}
#[test]
fn custom_extension_is_preserved_through_all_stages() {
    let mut fixture = Fixture::new();
    fixture.config.file_prefix = "application.jsonl".into();
    fs::write(
        fixture.path("application.segment.2020-01-01-01-00-00.1.jsonl"),
        b"one\n",
    )
    .unwrap();
    let mut store = fixture.store();
    assert!(drain(&mut store, now("2020-02-03")).is_empty());
    assert_eq!(
        read(&fixture.path("application.monthly.2020-01.part0001.jsonl.gz")),
        b"one\n"
    );
}

#[test]
fn late_fragment_on_same_day_uses_new_daily_and_monthly_names_after_cleanup() {
    let fixture = Fixture::new();
    let clock = now("2020-02-03");
    fixture.gzip("latest.segment.2020-01-01-01-00-00.1.log.gz", b"first\n");
    let mut store = fixture.store();
    assert!(drain(&mut store, clock).is_empty());
    fixture.gzip("latest.segment.2020-01-01-02-00-00.2.log.gz", b"late\n");
    store.request();
    assert!(drain(&mut store, clock).is_empty());
    assert_eq!(fixture.files(".monthly.").len(), 2);
    assert_eq!(
        fixture
            .files(".monthly.")
            .iter()
            .flat_map(|path| read(path))
            .collect::<Vec<_>>(),
        b"first\nlate\n"
    );
}

fn prepared_publication(fixture: &Fixture, published: bool) -> (PathBuf, PathBuf) {
    fixture.gzip("latest.daily.2020-01-01.1.log.gz", b"input\n");
    let store = fixture.store();
    let candidate = store.parse("latest.daily.2020-01-01.1.log.gz").unwrap();
    let path = store
        .create(Stage::Monthly, "2020-01".into(), &[candidate])
        .unwrap();
    let mut job = Job::load(&store.directory, path).unwrap();
    let temporary = job.temporary();
    fixture.gzip(temporary.to_str().unwrap(), b"input\n");
    let bytes = fs::read(&temporary).unwrap();
    job.manifest.pending = Some(Output {
        name: "latest.monthly.2020-01.part0001.log.gz".into(),
        sha256: hex(&Sha256::digest(&bytes)),
        length: bytes.len() as u64,
        end: Cursor {
            source: 1,
            offset: 0,
        },
    });
    job.persist().unwrap();
    let target = fixture.path("latest.monthly.2020-01.part0001.log.gz");
    if published {
        fs::rename(&temporary, &target).unwrap();
    }
    (temporary, target)
}
#[test]
fn prepared_output_is_recovered_before_or_after_its_atomic_publication() {
    for published in [false, true] {
        let fixture = Fixture::new();
        let (_, target) = prepared_publication(&fixture, published);
        let mut store = fixture.store();
        assert!(drain(&mut store, now("2020-02-03")).is_empty());
        assert_eq!(read(&target), b"input\n");
        assert_eq!(fixture.files(".monthly.").len(), 1);
        assert!(!fixture.path("latest.daily.2020-01-01.1.log.gz").exists());
    }
}
#[test]
fn incomplete_temporary_output_cannot_authorize_source_deletion() {
    let fixture = Fixture::new();
    let (temporary, target) = prepared_publication(&fixture, false);
    fs::write(temporary, b"incomplete").unwrap();
    let mut store = fixture.store();
    assert!(!drain(&mut store, now("2020-02-03")).is_empty());
    assert!(!target.exists());
    assert!(fixture.path("latest.daily.2020-01-01.1.log.gz").exists());
}
fn legacy_manifest(fixture: &Fixture, published: bool) -> (PathBuf, PathBuf, Vec<String>) {
    let directory = fixture.path(&format!(
        ".service-daemon-archive-transactions/{}-hourly",
        hex(b"latest.log")
    ));
    fs::create_dir_all(&directory).unwrap();
    let staging = directory.join("1-2.sources");
    fs::create_dir_all(&staging).unwrap();
    let names: Vec<_> = (1..=3)
        .map(|index| format!("latest.log.2020-01-01-{index:02}.gz"))
        .collect();
    for (index, name) in names.iter().enumerate() {
        fixture.gzip(name, format!("{index}\n").as_bytes());
        fs::copy(fixture.path(name), staging.join(format!("{index}.source"))).unwrap();
    }
    let target = fixture.path("latest.log.2020-01-01.merged.gz");
    if published {
        let mut file = File::create(&target).unwrap();
        for name in &names {
            file.write_all(&fs::read(fixture.path(name)).unwrap())
                .unwrap();
        }
        file.sync_all().unwrap();
    }
    let text = format!(
        "phase={}\ntarget={}\ntemporary={}\nstaging={}\nsource-count=3\n{}end\n",
        if published { "published" } else { "prepared" },
        hex(b"latest.log.2020-01-01.merged.gz"),
        hex(b"1-2.tmp"),
        hex(b"1-2.sources"),
        names
            .iter()
            .map(|name| format!("source={}\n", hex(name.as_bytes())))
            .collect::<String>()
    );
    let manifest = directory.join("1-2.manifest");
    fs::write(&manifest, text).unwrap();
    (manifest, target, names)
}
#[test]
fn legacy_published_transaction_restores_missing_inputs_then_migrates_without_duplication() {
    let fixture = Fixture::new();
    let (manifest, _, names) = legacy_manifest(&fixture, true);
    fs::remove_file(fixture.path(&names[0])).unwrap();
    let mut store = fixture.store();
    assert!(drain(&mut store, now("2020-02-03")).is_empty());
    assert!(!manifest.exists());
    assert_eq!(
        fixture
            .files(".monthly.")
            .iter()
            .flat_map(|path| read(path))
            .collect::<Vec<_>>(),
        b"0\n1\n2\n"
    );
}
#[test]
fn legacy_restore_does_not_overwrite_an_existing_complete_source() {
    let fixture = Fixture::new();
    let (manifest, _, names) = legacy_manifest(&fixture, false);
    fixture.gzip(&names[0], b"newer complete source\n");
    let original = fs::read(fixture.path(&names[0])).unwrap();
    recover_legacy(Path::new(&fixture.config.directory), "latest.log").unwrap();
    assert_eq!(fs::read(fixture.path(&names[0])).unwrap(), original);
    assert!(!manifest.exists());
}
#[test]
fn malformed_legacy_transaction_is_retained_without_blocking_unrelated_date() {
    let fixture = Fixture::new();
    let (manifest, _, _) = legacy_manifest(&fixture, false);
    let mut text = fs::read_to_string(&manifest).unwrap();
    text.truncate(text.len() - 4);
    fs::write(&manifest, text).unwrap();
    fixture.gzip("latest.2020-01-02.log.gz", b"independent\n");
    let mut store = fixture.store();
    assert!(drain(&mut store, now("2020-01-05")).is_empty());
    assert!(manifest.exists());
    assert_eq!(read(&fixture.files(".daily.")[0]), b"independent\n");
}

#[cfg(windows)]
#[test]
fn windows_source_delete_failure_keeps_published_archive_and_retries_safely() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    let name = "latest.2020-01-01.log";
    fs::write(fixture.path(name), b"one\n").unwrap();
    let handle = OpenOptions::new()
        .read(true)
        .share_mode(0x00000003)
        .open(fixture.path(name))
        .unwrap();
    let mut store = fixture.store();
    assert!(!drain(&mut store, now("2020-01-01")).is_empty());
    assert!(fixture.path(name).exists());
    assert_eq!(read(&fixture.path(&format!("{name}.gz"))), b"one\n");
    drop(handle);
    drop(store);
    let mut store = fixture.store();
    assert!(drain(&mut store, now("2020-01-01")).is_empty());
    assert!(!fixture.path(name).exists());
}

#[test]
fn empty_inputs_get_a_valid_archive_and_are_cleaned() {
    let fixture = Fixture::new();
    let name = "latest.segment.2020-01-01-01-00-00.1.log";
    fs::write(fixture.path(name), []).unwrap();
    let mut store = fixture.store();
    assert!(drain(&mut store, now("2020-02-03")).is_empty());
    assert!(!fixture.path(name).exists());
    let volumes = fixture.files(".monthly.");
    assert_eq!(volumes.len(), 1);
    assert!(read(&volumes[0]).is_empty());
    for entry in fs::read_dir(&store.transactions).unwrap().flatten() {
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let manifest: Manifest =
                serde_json::from_slice(&fs::read(entry.path()).unwrap()).unwrap();
            manifest.validate().unwrap();
            assert!(manifest.cleaned);
        }
    }
}

#[test]
fn volume_sealed_exactly_at_final_newline_advances_cleanup_range() {
    let mut fixture = Fixture::new();
    fixture.config.archive.monthly_volume_size = 1;
    // Gzip writes its header on the first block. The second block is one very
    // long complete line, so the first split falls exactly at input EOF.
    let mut input = vec![b'x'; BLOCK * 2];
    input.push(b'\n');
    let name = "latest.daily.2020-01-01.1.log.gz";
    fixture.gzip(name, &input);
    let mut store = fixture.store();
    assert!(drain(&mut store, now("2020-02-03")).is_empty());
    assert!(!fixture.path(name).exists());
    let volumes = fixture.files(".monthly.");
    assert_eq!(volumes.len(), 1);
    assert_eq!(read(&volumes[0]), input);
}

#[test]
fn pending_writer_handoff_is_not_compressed_before_the_journal_commits() {
    let fixture = Fixture::new();
    let name = "latest.segment.2020-01-01-01-00-00.1.log";
    fs::write(fixture.path(name), b"closed\n").unwrap();
    fs::write(
        super::super::rotation::state_path(&fixture.config),
        serde_json::to_vec(&serde_json::json!({
            "id":"test", "sequence":1,"started":SystemTime::now(),"deadline":null,
            "date":"2020-01-01","stamp":"2020-01-01-01-00-00","pending":name
        }))
        .unwrap(),
    )
    .unwrap();
    let mut store = fixture.store();
    assert!(drain(&mut store, now("2020-02-03")).is_empty());
    assert!(fixture.path(name).exists());
    assert!(!fixture.path(&format!("{name}.gz")).exists());
    fs::remove_file(super::super::rotation::state_path(&fixture.config)).unwrap();
    store.request();
    assert!(drain(&mut store, now("2020-02-03")).is_empty());
    assert_eq!(read(&fixture.files(".monthly.")[0]), b"closed\n");
}

#[test]
fn damaged_transaction_quarantines_its_day_but_not_an_independent_day() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let name = "latest.2020-01-01.log.gz";
    fixture.gzip(name, b"retained\n");
    let candidate = store.parse(name).unwrap();
    let path = store
        .create(Stage::Daily, "2020-01-01".into(), &[candidate])
        .unwrap();
    let mut bytes = fs::read(&path).unwrap();
    bytes.truncate(bytes.len() - 2);
    fs::write(&path, bytes).unwrap();
    fixture.gzip("latest.2020-01-02.log.gz", b"independent\n");
    let mut store = fixture.store();
    assert!(drain(&mut store, now("2020-01-05")).is_empty());
    assert!(fixture.path(name).exists());
    assert!(path.exists());
    assert_eq!(read(&fixture.files(".daily.")[0]), b"independent\n");
}

#[test]
fn publication_failure_preserves_pending_output_and_recovers_without_duplication() {
    let fixture = Fixture::new();
    let (temporary, target) = prepared_publication(&fixture, false);
    fs::create_dir(&target).unwrap();
    let mut store = fixture.store();
    assert!(!drain(&mut store, now("2020-02-03")).is_empty());
    assert!(temporary.exists());
    assert!(fixture.path("latest.daily.2020-01-01.1.log.gz").exists());
    fs::remove_dir(target).unwrap();
    drop(store);
    assert!(drain(&mut fixture.store(), now("2020-02-03")).is_empty());
    assert_eq!(fixture.files(".monthly.").len(), 1);
}

#[test]
fn temporary_write_failure_preserves_sources_and_is_retryable() {
    let fixture = Fixture::new();
    let name = "latest.daily.2020-01-01.1.log.gz";
    fixture.gzip(name, b"retained\n");
    let store = fixture.store();
    let path = store
        .create(
            Stage::Monthly,
            "2020-01".into(),
            &[store.parse(name).unwrap()],
        )
        .unwrap();
    fs::create_dir(path.with_extension("gz.tmp")).unwrap();
    let mut store = fixture.store();
    assert!(!drain(&mut store, now("2020-02-03")).is_empty());
    assert!(fixture.path(name).exists());
    fs::remove_dir(path.with_extension("gz.tmp")).unwrap();
    drop(store);
    assert!(drain(&mut fixture.store(), now("2020-02-03")).is_empty());
    assert_eq!(read(&fixture.files(".monthly.")[0]), b"retained\n");
}

fn assert_retry_preserves_batch_order(stage: Stage) {
    let fixture = Fixture::new();
    let (first, late, unrelated, date, related_output, unrelated_output, clock) = match stage {
        Stage::Daily => (
            "latest.segment.2020-01-01-01-00-00.1.log.gz",
            "latest.segment.2020-01-01-02-00-00.2.log.gz",
            "latest.segment.2020-01-02-01-00-00.3.log.gz",
            "2020-01-01",
            ".daily.2020-01-01.",
            ".daily.2020-01-02.",
            now("2020-01-05"),
        ),
        Stage::Monthly => (
            "latest.daily.2020-01-01.1.log.gz",
            "latest.daily.2020-01-02.1.log.gz",
            "latest.daily.2020-02-01.1.log.gz",
            "2020-01",
            ".monthly.2020-01.",
            ".monthly.2020-02.",
            now("2020-03-03"),
        ),
        Stage::Fragment => unreachable!(),
    };
    fixture.gzip(first, b"first\n");
    let store = fixture.store();
    let path = store
        .create(stage, date.into(), &[store.parse(first).unwrap()])
        .unwrap();
    fs::create_dir(path.with_extension("gz.tmp")).unwrap();
    let mut store = fixture.store();
    assert!(!drain(&mut store, clock).is_empty());
    fixture.gzip(late, b"late\n");
    fixture.gzip(unrelated, b"unrelated\n");
    store.request();
    assert!(drain(&mut store, clock).is_empty());
    assert_eq!(fixture.files(unrelated_output).len(), 1);
    assert!(fixture.files(related_output).is_empty());
    assert!(fixture.path(late).exists());
    fs::remove_dir(path.with_extension("gz.tmp")).unwrap();
    drop(store);
    assert!(drain(&mut fixture.store(), clock).is_empty());
    let outputs = fixture.files(related_output);
    assert_eq!(outputs.len(), 2);
    assert_eq!(read(&outputs[0]), b"first\n");
    assert_eq!(read(&outputs[1]), b"late\n");
}

#[test]
fn daily_retry_blocks_late_same_day_input_but_allows_other_days() {
    assert_retry_preserves_batch_order(Stage::Daily);
}

#[test]
fn monthly_retry_blocks_late_same_month_input_but_allows_other_months() {
    assert_retry_preserves_batch_order(Stage::Monthly);
}

#[test]
fn each_stage_uses_its_own_publicly_configured_level() {
    let mut fixture = Fixture::new();
    fixture.config.archive.fragment_compression_level = 9;
    fixture.config.archive.daily_compression_level = 1;
    fixture.config.archive.monthly_compression_level = 6;
    let name = "latest.segment.2020-01-01-01-00-00.1.log";
    let input = corpus(300);
    fs::write(fixture.path(name), &input).unwrap();
    let mut store = fixture.store();
    assert!(drain(&mut store, now("2020-01-01")).is_empty());
    assert_eq!(fs::read(fixture.path(&format!("{name}.gz"))).unwrap()[8], 2);
    store.request();
    assert!(drain(&mut store, now("2020-01-03")).is_empty());
    assert_eq!(fs::read(&fixture.files(".daily.")[0]).unwrap()[8], 4);
    store.request();
    assert!(drain(&mut store, now("2020-02-03")).is_empty());
    let volume = &fixture.files(".monthly.")[0];
    assert_eq!(fs::read(volume).unwrap()[8], 0);
    assert_eq!(read(volume), input);
}

#[test]
fn restart_during_legacy_input_cleanup_rebuilds_from_backups_without_duplication() {
    let fixture = Fixture::new();
    let (manifest, _, names) = legacy_manifest(&fixture, true);
    let mut store = fixture.store();
    for _ in 0..100 {
        store
            .step(now("2020-02-03"), &AtomicBool::new(false))
            .unwrap();
        if !fixture.path(&names[0]).exists() {
            break;
        }
    }
    assert!(!fixture.path(&names[0]).exists());
    assert!(manifest.exists());
    drop(store);
    assert!(drain(&mut fixture.store(), now("2020-02-03")).is_empty());
    assert_eq!(
        fixture
            .files(".monthly.")
            .iter()
            .flat_map(|path| read(path))
            .collect::<Vec<_>>(),
        b"0\n1\n2\n"
    );
}

#[test]
fn legacy_validation_is_incremental_and_new_fragments_preempt_it() {
    let fixture = Fixture::new();
    let (manifest, target, names) = legacy_manifest(&fixture, true);
    fixture.gzip(&names[0], &corpus(5000));
    let mut output = File::create(target).unwrap();
    for name in &names {
        output
            .write_all(&fs::read(fixture.path(name)).unwrap())
            .unwrap();
    }
    drop(output);
    let mut store = fixture.store();
    for _ in 0..10 {
        store
            .step(now("2020-02-03"), &AtomicBool::new(false))
            .unwrap();
    }
    let name = "latest.segment.2020-02-03-01-00-00.10.log";
    fs::write(fixture.path(name), b"urgent\n").unwrap();
    store.request();
    for _ in 0..30 {
        store
            .step(now("2020-02-03"), &AtomicBool::new(false))
            .unwrap();
        if fixture.path(&format!("{name}.gz")).exists() {
            break;
        }
    }
    assert!(fixture.path(&format!("{name}.gz")).exists());
    assert!(manifest.exists());
    assert!(
        !store
            .step(now("2020-02-03"), &AtomicBool::new(true))
            .unwrap()
    );
}

#[test]
fn damaged_existing_legacy_source_retains_both_source_and_backup_for_recovery() {
    let fixture = Fixture::new();
    let (manifest, _, names) = legacy_manifest(&fixture, false);
    let staging = manifest.parent().unwrap().join("1-2.sources/0.source");
    fs::write(fixture.path(&names[0]), b"damaged gzip").unwrap();
    fixture.gzip("latest.2020-01-02.log.gz", b"independent\n");
    let mut store = fixture.store();
    assert!(!drain(&mut store, now("2020-01-05")).is_empty());
    assert!(manifest.exists());
    assert_eq!(read(&staging), b"0\n");
    assert_eq!(fs::read(fixture.path(&names[0])).unwrap(), b"damaged gzip");
    assert_eq!(read(&fixture.files(".daily.")[0]), b"independent\n");
}

#[cfg(unix)]
#[test]
fn daily_grace_is_elapsed_time_across_os_dst_change() {
    const FLAG: &str = "SD_LOG_TEST_DAILY_GRACE_DST";
    if std::env::var_os(FLAG).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "core::logging::archive::tests::daily_grace_is_elapsed_time_across_os_dst_change",
                "--nocapture",
            ])
            .env("TZ", "America/New_York")
            .env(FLAG, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let fixture = Fixture::new();
    fixture.gzip("latest.2020-03-07.log.gz", b"before DST\n");
    let mut store = fixture.store();
    let before: SystemTime = chrono::DateTime::parse_from_rfc3339("2020-03-09T00:30:00-04:00")
        .unwrap()
        .into();
    assert!(drain(&mut store, before).is_empty());
    assert!(fixture.files(".daily.").is_empty());
    store.request();
    assert!(drain(&mut store, before + Duration::from_secs(1800)).is_empty());
    assert_eq!(read(&fixture.files(".daily.")[0]), b"before DST\n");
}

#[test]
fn complete_unpublished_legacy_output_recovers_its_existing_archive_prefix() {
    let fixture = Fixture::new();
    let (manifest, _, names) = legacy_manifest(&fixture, false);
    let temporary = manifest.parent().unwrap().join("1-2.tmp");
    fixture.gzip(temporary.to_str().unwrap(), b"earlier archive\n");
    let mut output = OpenOptions::new().append(true).open(temporary).unwrap();
    for name in names {
        output
            .write_all(&fs::read(fixture.path(&name)).unwrap())
            .unwrap();
    }
    drop(output);
    assert!(drain(&mut fixture.store(), now("2020-02-03")).is_empty());
    assert!(!manifest.exists());
    assert_eq!(
        read(&fixture.files(".monthly.")[0]),
        b"earlier archive\n0\n1\n2\n"
    );
}

#[test]
fn incomplete_unpublished_legacy_output_is_retained_with_all_recovery_sources() {
    let fixture = Fixture::new();
    let (manifest, _, names) = legacy_manifest(&fixture, false);
    let temporary = manifest.parent().unwrap().join("1-2.tmp");
    fs::write(&temporary, b"partial output").unwrap();
    assert!(!drain(&mut fixture.store(), now("2020-02-03")).is_empty());
    assert!(manifest.exists());
    assert!(temporary.exists());
    assert!(names.iter().all(|name| fixture.path(name).exists()));
}
