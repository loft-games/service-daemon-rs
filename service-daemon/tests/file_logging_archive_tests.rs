#![cfg(feature = "file-logging")]

//! Public integration coverage for the file-log archive lifecycle.
//!
//! `enable_file_logging` intentionally has process-global, one-shot
//! configuration.  Each scenario therefore runs the daemon in a child test
//! process so that the scenarios remain independent when the test harness
//! executes them in parallel.

use anyhow::Context;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use service_daemon::{
    DaemonLayer, FileLogCompression, FileLogConfig, Registry, RestartPolicy, RotationPolicy,
    ServiceDaemon, ServiceStatus, enable_file_logging,
};
use tracing_subscriber::prelude::*;

const CHILD_MODE: &str = "SD_FILE_LOG_ARCHIVE_CHILD";
const CHILD_DIR: &str = "SD_FILE_LOG_ARCHIVE_DIR";
const TEST_NAME: &str = "file_logging_archive_lifecycle";
const WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const SERVICE_INSTANCE_ID: &str = "019fe746-6158-7403-82c9-ac72cad515ec";
const SOURCE_SERVICE_INSTANCE_ID: &str = "019fe746-6158-7403-82c9-ac6ccaa91ef5";
const MESSAGE_ID: &str = "019fe746-6158-7403-82c9-ac8b7e502cb3";

fn temporary_directory(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!(
        "service-daemon-file-archive-{label}-{}-{nonce}",
        std::process::id()
    ))
}

fn run_child(mode: &str, directory: &Path) -> Output {
    Command::new(std::env::current_exe().expect("test executable path"))
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(CHILD_MODE, mode)
        .env(CHILD_DIR, directory)
        .output()
        .expect("spawn file logging child")
}

fn write_old_fragment(path: &Path, body: &str) -> std::io::Result<()> {
    fs::write(path, body)?;
    let old = SystemTime::now()
        .checked_sub(Duration::from_secs(31 * 24 * 60 * 60))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    OpenOptions::new().write(true).open(path)?.set_modified(old)
}

fn write_old_gzip_fragment(path: &Path, body: &str) -> std::io::Result<()> {
    let output = File::create(path)?;
    let mut encoder = flate2::write::GzEncoder::new(output, flate2::Compression::default());
    encoder.write_all(body.as_bytes())?;
    encoder.finish()?.sync_all()?;
    let old = SystemTime::now()
        .checked_sub(Duration::from_secs(31 * 24 * 60 * 60))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    OpenOptions::new().write(true).open(path)?.set_modified(old)
}

fn assert_child_success(output: &Output) {
    assert!(
        output.status.success(),
        "child failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn read_gzip(path: &Path) -> anyhow::Result<String> {
    let mut contents = String::new();
    flate2::read::MultiGzDecoder::new(
        File::open(path).with_context(|| format!("open archive {}", path.display()))?,
    )
    .read_to_string(&mut contents)
    .with_context(|| format!("decode archive {}", path.display()))?;
    Ok(contents)
}

fn write_historical_active(path: &Path, body: &str) -> std::io::Result<()> {
    fs::write(path, body)?;
    // 2020-01-01 01:00 UTC gives a deterministic closed daily/hourly period.
    OpenOptions::new()
        .write(true)
        .open(path)?
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_577_840_400))
}

fn assert_active_markers(path: &Path, expected_count: usize) -> anyhow::Result<()> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("read active log {}", path.display()))?;
    let markers = contents
        .lines()
        .filter(|line| line.contains("archive_test_marker"))
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(markers.len(), expected_count, "{contents}");
    for marker in markers {
        assert_eq!(marker["service_instance_id"], SERVICE_INSTANCE_ID);
        assert_eq!(
            marker["source_service_instance_id"],
            SOURCE_SERVICE_INSTANCE_ID
        );
        assert_eq!(marker["message_id"], MESSAGE_ID);
        assert_eq!(
            marker["trigger_instance_id"]["service_instance_id"],
            SERVICE_INSTANCE_ID
        );
        assert_eq!(marker["trigger_instance_id"]["seq"], 42);
    }
    Ok(())
}

/// A bounded condition wait, not a fixed processing allowance.
async fn wait_until(
    description: &str,
    mut ready: impl FnMut() -> anyhow::Result<bool>,
) -> anyhow::Result<()> {
    tokio::time::timeout(WAIT_TIMEOUT, async {
        loop {
            if ready()? {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .with_context(|| format!("timed out waiting for {description}"))?
}

fn marker_persisted(directory: &Path, prefix: &str, marker: &str) -> anyhow::Result<bool> {
    // A later framework record may rotate the marker out of the active file.
    // Closed fragments may also be compressed between discovery and opening.
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let stem = prefix.rsplit_once('.').map_or(prefix, |(stem, _)| stem);
        if (name != prefix && !name.starts_with(&format!("{stem}.segment."))) || !path.is_file() {
            continue;
        }
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        let mut decoded = Vec::new();
        let contents = if path.extension().is_some_and(|extension| extension == "gz") {
            flate2::read::MultiGzDecoder::new(bytes.as_slice())
                .read_to_end(&mut decoded)
                .with_context(|| format!("decode {}", path.display()))?;
            decoded.as_slice()
        } else {
            bytes.as_slice()
        };
        // Only complete lines count; the active writer may be mid-record.
        for line in contents.split_inclusive(|byte| *byte == b'\n') {
            if !line.ends_with(b"\n") {
                continue;
            }
            // Historical fixture text is deliberately not JSON.
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(line)
                && value["message"] == marker
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

async fn wait_for_size_records(directory: &Path, prefix: &str, marker: &str) -> anyhow::Result<()> {
    let description = format!(
        "record drain and archive cleanup in {}",
        directory.display()
    );
    let mut records_persisted = false;
    wait_until(&description, || {
        if !records_persisted {
            records_persisted = marker_persisted(directory, prefix, marker)?;
        }
        // Gzip publication and source deletion are separate archive steps.
        // Wait for cleanup before shutdown can leave both copies to count.
        Ok(records_persisted
            && matching(directory, ".segment.")?.iter().all(|fragment| {
                fragment
                    .extension()
                    .is_some_and(|extension| extension == "gz")
            }))
    })
    .await
}

fn run_child_scenario(mode: &str, directory: &Path) -> anyhow::Result<()> {
    let mut config = match mode {
        "default" => FileLogConfig {
            directory: directory.to_string_lossy().into_owned(),
            ..FileLogConfig::default()
        },
        "custom" => FileLogConfig::new(directory.to_string_lossy(), "application.log"),
        "size" => FileLogConfig {
            max_file_size: Some(512),
            ..FileLogConfig::new(directory.to_string_lossy(), "latest.log")
        },
        "custom-bare" => FileLogConfig::new(directory.to_string_lossy(), "app"),
        "archive" => FileLogConfig {
            directory: directory.to_string_lossy().into_owned(),
            file_prefix: "latest.log".to_string(),
            rotation: RotationPolicy::Hourly,
            compression: FileLogCompression::Gzip,
            ..FileLogConfig::default()
        },
        "none" => FileLogConfig {
            directory: directory.to_string_lossy().into_owned(),
            file_prefix: "custom.log".to_string(),
            rotation: RotationPolicy::Hourly,
            compression: FileLogCompression::None,
            ..FileLogConfig::default()
        },
        "never" => FileLogConfig {
            directory: directory.to_string_lossy().into_owned(),
            file_prefix: "never.log".to_string(),
            rotation: RotationPolicy::Never,
            compression: FileLogCompression::Gzip,
            ..FileLogConfig::default()
        },
        _ => anyhow::bail!("unknown file logging child mode {mode:?}"),
    };

    // These lifecycle cases exercise processing without real scheduling waits.
    config.archive.daily_start_jitter = Duration::ZERO;
    config.archive.monthly_start_jitter = Duration::ZERO;
    config.archive.monthly_volume_jitter = Duration::ZERO;
    let prefix = config.file_prefix.clone();
    let archive_count = match mode {
        "archive" => 2,
        "default" | "custom" | "custom-bare" => 1,
        _ => 0,
    };
    let historical_sources: Vec<_> = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|path| {
            path.is_file()
                && path.file_name().is_some_and(|name| {
                    let name = name.to_string_lossy();
                    name != prefix && !name.starts_with('.') && !name.contains(".monthly.")
                })
        })
        .collect();
    enable_file_logging(config);
    tracing_subscriber::registry()
        .with(DaemonLayer)
        .try_init()
        .map_err(|error| anyhow::anyhow!("install test subscriber: {error}"))?;
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let daemon = ServiceDaemon::builder()
            .with_registry(Registry::builder().with_tag("__file_log__").build())
            .with_restart_policy(RestartPolicy {
                // Exercise the readiness barrier even when startup stops waiting immediately.
                wave_spawn_timeout: Duration::ZERO,
                ..RestartPolicy::for_testing()
            })
            .build();
        daemon.run().await;
        let persisted = async {
            let instances = daemon.service_instances();
            anyhow::ensure!(instances.len() == 1, "expected exactly one file log service");
            // run() may return after its startup timeout, before the sink subscribes.
            tokio::time::timeout(WAIT_TIMEOUT, async {
                while instances[0].status().await != ServiceStatus::Healthy {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .context("file log service did not become Healthy")?;
            let drain_marker = format!("archive_drain_{}", uuid::Uuid::now_v7());
            {
                let span = tracing::info_span!(
                    "archive_identity",
                    service_instance_id = SERVICE_INSTANCE_ID,
                    source_service_instance_id = SOURCE_SERVICE_INSTANCE_ID,
                    message_id = MESSAGE_ID,
                    instance_seq = 42_u64,
                );
                let _entered = span.enter();
                let count = if mode == "size" { 40 } else { 1 };
                for index in 0..count {
                    tracing::info!(target: "file_logging.archive.integration", index, "archive_test_marker");
                }
                tracing::info!(target: "file_logging.archive.integration", "{drain_marker}");
            }
            if mode == "size" {
                wait_for_size_records(directory, &prefix, &drain_marker).await?;
            } else {
                let marker_wait = format!("marker {drain_marker} in {}", directory.display());
                wait_until(&marker_wait, || {
                    marker_persisted(directory, &prefix, &drain_marker)
                })
                .await?;
            }
            if archive_count > 0 {
                // Graceful shutdown is bounded and may leave recoverable work.
                // These normal-completion cases must finish before shutdown.
                let archive_wait = format!(
                    "{archive_count} monthly archives and source cleanup in {}",
                    directory.display()
                );
                wait_until(&archive_wait, || {
                    Ok(matching(directory, ".monthly.")?.len() == archive_count
                        && historical_sources.iter().all(|path| !path.exists()))
                })
                .await?;
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(error) = &persisted {
            eprintln!("file log scenario {mode} failed: {error:#}");
        }
        daemon.shutdown();
        let shutdown = daemon.wait().await;
        if let Err(error) = &shutdown {
            eprintln!("file log scenario {mode} shutdown failed: {error:#}");
        }
        persisted?;
        shutdown?;
        Ok(())
    })
}

fn matching(directory: &Path, marker: &str) -> anyhow::Result<Vec<PathBuf>> {
    let mut paths: Vec<_> = fs::read_dir(directory)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().contains(marker))
        })
        .collect();
    paths.sort();
    Ok(paths)
}

#[test]
fn file_logging_archive_lifecycle() -> anyhow::Result<()> {
    if let Ok(mode) = std::env::var(CHILD_MODE) {
        let directory = PathBuf::from(
            std::env::var_os(CHILD_DIR)
                .ok_or_else(|| anyhow::anyhow!("missing child directory"))?,
        );
        return run_child_scenario(&mode, &directory);
    }
    let directory = temporary_directory("legacy-pipeline");
    fs::create_dir_all(&directory)?;
    write_old_fragment(&directory.join("latest.log.2019-12-31-23"), "raw\n")?;
    for hour in 1..=3 {
        write_old_gzip_fragment(
            &directory.join(format!("latest.2020-01-01-{hour:02}.log.gz")),
            &format!("fragment {hour}\n"),
        )?;
    }
    let merged = directory.join("latest.log.2019-12-30.merged.gz");
    write_old_gzip_fragment(&merged, "first member\n")?;
    let mut encoder = flate2::write::GzEncoder::new(
        OpenOptions::new().append(true).open(&merged)?,
        flate2::Compression::default(),
    );
    encoder.write_all(b"second member\n")?;
    encoder.finish()?.sync_all()?;
    let marker = directory.join("latest.log.2019-12-30.merged.gz.needs-repack");
    write_old_fragment(&marker, "needs repack\n")?;
    assert_child_success(&run_child("archive", &directory));
    let volumes = matching(&directory, ".monthly.")?;
    assert_eq!(volumes.len(), 2);
    assert_eq!(
        read_gzip(&volumes[0])?,
        "first member\nsecond member\nraw\n"
    );
    assert_eq!(
        read_gzip(&volumes[1])?,
        "fragment 1\nfragment 2\nfragment 3\n"
    );
    assert!(!marker.exists());
    assert!(!merged.exists());
    assert_active_markers(&directory.join("latest.log"), 1)?;
    let before: Vec<_> = volumes.iter().map(fs::read).collect::<Result<_, _>>()?;
    assert_child_success(&run_child("archive", &directory));
    assert_eq!(
        volumes
            .iter()
            .map(fs::read)
            .collect::<Result<Vec<_>, _>>()?,
        before
    );
    assert_active_markers(&directory.join("latest.log"), 2)?;
    fs::remove_dir_all(directory)?;
    Ok(())
}

#[test]
fn stale_active_file_is_rotated_and_same_interval_restart_only_appends() -> anyhow::Result<()> {
    let directory = temporary_directory("restart");
    fs::create_dir_all(&directory)?;
    write_historical_active(&directory.join("latest.log"), "historical\n")?;
    assert_child_success(&run_child("default", &directory));
    let volumes = matching(&directory, ".monthly.")?;
    assert_eq!(volumes.len(), 1);
    assert_eq!(read_gzip(&volumes[0])?, "historical\n");
    assert_active_markers(&directory.join("latest.log"), 1)?;
    let state_before: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join(".latest.log.active.json"))?)?;
    assert_child_success(&run_child("default", &directory));
    let state_after: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join(".latest.log.active.json"))?)?;
    assert_eq!(state_before, state_after);
    assert_active_markers(&directory.join("latest.log"), 2)?;
    fs::remove_dir_all(directory)?;
    Ok(())
}

#[test]
fn custom_and_extensionless_names_keep_their_active_filename() -> anyhow::Result<()> {
    for (mode, prefix, old, output) in [
        (
            "custom",
            "application.log",
            "application.log.2020-01-01",
            "application.monthly.2020-01.part0001.log.gz",
        ),
        (
            "custom-bare",
            "app",
            "app.2020-01-01",
            "app.monthly.2020-01.part0001.gz",
        ),
    ] {
        let directory = temporary_directory(mode);
        fs::create_dir_all(&directory)?;
        write_old_fragment(&directory.join(old), "custom\n")?;
        assert_child_success(&run_child(mode, &directory));
        assert_eq!(read_gzip(&directory.join(output))?, "custom\n");
        assert_active_markers(&directory.join(prefix), 1)?;
        fs::remove_dir_all(directory)?;
    }
    Ok(())
}

#[test]
fn none_rotates_without_archiving_and_never_disables_both_conditions() -> anyhow::Result<()> {
    for (mode, prefix) in [("none", "custom.log"), ("never", "never.log")] {
        let directory = temporary_directory(mode);
        fs::create_dir_all(&directory)?;
        write_historical_active(&directory.join(prefix), "historical\n")?;
        assert_child_success(&run_child(mode, &directory));
        assert!(matching(&directory, ".gz")?.is_empty());
        let fragments = matching(&directory, ".segment.")?;
        if mode == "none" {
            assert_eq!(fragments.len(), 1);
            assert_eq!(fs::read_to_string(&fragments[0])?, "historical\n");
        } else {
            assert!(fragments.is_empty());
            assert!(fs::read_to_string(directory.join(prefix))?.starts_with("historical\n"));
        }
        assert_active_markers(&directory.join(prefix), 1)?;
        fs::remove_dir_all(directory)?;
    }
    Ok(())
}

#[test]
fn size_rotation_keeps_each_queued_json_record_intact() -> anyhow::Result<()> {
    let directory = temporary_directory("size");
    fs::create_dir_all(&directory)?;
    let output = run_child("size", &directory);
    assert_child_success(&output);
    let fragments = matching(&directory, ".segment.")?;
    assert!(fragments.len() > 1);
    let mut markers = 0;
    for path in fragments
        .into_iter()
        .chain(std::iter::once(directory.join("latest.log")))
    {
        let text = if path.extension().is_some_and(|value| value == "gz") {
            read_gzip(&path)?
        } else {
            fs::read_to_string(path)?
        };
        for line in text.lines() {
            let value: serde_json::Value = serde_json::from_str(line)?;
            if value["message"]
                .as_str()
                .is_some_and(|message| message.contains("archive_test_marker"))
            {
                markers += 1;
            }
        }
    }
    assert_eq!(
        markers,
        40,
        "child stdout:\n{}\nchild stderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(directory)?;
    Ok(())
}

#[tokio::test]
async fn size_record_drain_waits_for_archive_source_cleanup() -> anyhow::Result<()> {
    let directory = temporary_directory("size-drain");
    fs::create_dir_all(&directory)?;
    let active = directory.join("latest.log");
    let drain_marker = "size-drain-marker";
    fs::write(
        &active,
        format!("{}\n", serde_json::json!({ "message": drain_marker })),
    )?;
    let fragment = directory.join("latest.segment.2020-01-01-01-00-00.1.log");
    let body = "{\"message\":\"archive_test_marker { index=0 }\"}\n";
    fs::write(&fragment, body)?;
    write_old_gzip_fragment(&fragment.with_extension("log.gz"), body)?;

    // Publication precedes source cleanup. A persisted drain marker alone
    // does not make it safe to count both raw and compressed fragments.
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            wait_for_size_records(&directory, "latest.log", drain_marker),
        )
        .await
        .is_err(),
        "drain finished while the source and its archive were both present"
    );
    fs::remove_file(fragment)?;
    wait_for_size_records(&directory, "latest.log", drain_marker).await?;
    fs::remove_dir_all(directory)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn rotation_dates_use_container_local_time_without_changing_event_timestamps() -> anyhow::Result<()>
{
    let directory = temporary_directory("timezone");
    fs::create_dir_all(&directory)?;
    write_historical_active(&directory.join("latest.log"), "historical\n")?;
    let output = Command::new(std::env::current_exe()?)
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(CHILD_MODE, "default")
        .env(CHILD_DIR, &directory)
        .env("TZ", "Etc/GMT+8")
        .output()?;
    assert_child_success(&output);
    assert_eq!(
        read_gzip(&directory.join("latest.monthly.2019-12.part0001.log.gz"))?,
        "historical\n"
    );
    let text = fs::read_to_string(directory.join("latest.log"))?;
    for line in text.lines() {
        let value: serde_json::Value = serde_json::from_str(line)?;
        assert!(
            value["timestamp"]
                .as_str()
                .is_some_and(|timestamp| timestamp.ends_with('Z') || timestamp.ends_with("+00:00"))
        );
    }
    fs::remove_dir_all(directory)?;
    Ok(())
}

#[test]
fn marker_observation_follows_rotation_and_compression() -> anyhow::Result<()> {
    for prefix in ["application.log", "app"] {
        let directory = temporary_directory("marker-observation");
        fs::create_dir_all(&directory)?;
        let active = directory.join(prefix);
        let marker = r#"{"message":"current-child-marker"}"#;
        fs::write(&active, marker)?;
        assert!(!marker_persisted(
            &directory,
            prefix,
            "current-child-marker"
        )?);
        let complete = format!("{marker}\n");
        fs::write(&active, &complete)?;
        assert!(marker_persisted(
            &directory,
            prefix,
            "current-child-marker"
        )?);

        let (stem, extension) = prefix
            .rsplit_once('.')
            .map_or((prefix, String::new()), |(stem, extension)| {
                (stem, format!(".{extension}"))
            });
        let segment_name = format!("{stem}.segment.2026-11-11-00-00-00.1{extension}");
        let segment = directory.join(&segment_name);
        fs::rename(&active, &segment)?;
        fs::write(&active, b"{\"message\":\"later framework record")?;
        assert!(marker_persisted(
            &directory,
            prefix,
            "current-child-marker"
        )?);

        let compressed = directory.join(format!("{segment_name}.gz"));
        write_old_gzip_fragment(&compressed, &complete)?;
        fs::remove_file(segment)?;
        assert!(marker_persisted(
            &directory,
            prefix,
            "current-child-marker"
        )?);
        assert!(!marker_persisted(&directory, prefix, "next-child-marker")?);
        fs::remove_dir_all(directory)?;
    }
    Ok(())
}
