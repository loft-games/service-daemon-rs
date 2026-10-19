#![cfg(feature = "file-logging")]

//! Public integration coverage for the file-log archive lifecycle.
//!
//! `enable_file_logging` intentionally has process-global, one-shot
//! configuration.  Each scenario therefore runs the daemon in a child test
//! process so that the scenarios remain independent when the test harness
//! executes them in parallel.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use service_daemon::{
    DaemonLayer, FileLogCompression, FileLogConfig, Registry, RestartPolicy, RotationPolicy,
    ServiceDaemon, enable_file_logging,
};
use tracing_subscriber::prelude::*;

const CHILD_MODE: &str = "SD_FILE_LOG_ARCHIVE_CHILD";
const CHILD_DIR: &str = "SD_FILE_LOG_ARCHIVE_DIR";
const TEST_NAME: &str = "file_logging_archive_lifecycle";
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

fn read_gzip(path: &Path) -> std::io::Result<String> {
    let mut contents = String::new();
    flate2::read::MultiGzDecoder::new(File::open(path)?).read_to_string(&mut contents)?;
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
    let contents = fs::read_to_string(path)?;
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

#[test]
fn file_log_config_defaults_to_application_compatible_name_and_gzip() {
    let config = FileLogConfig::default();
    assert_eq!(config.directory, "logs");
    assert_eq!(config.file_prefix, "latest.log");
    assert!(matches!(config.rotation, RotationPolicy::Daily));
    assert!(matches!(config.compression, FileLogCompression::Gzip));
    let custom = FileLogConfig::new("custom-directory", "application.log");
    assert_eq!(custom.directory, "custom-directory");
    assert_eq!(custom.file_prefix, "application.log");
}

#[test]
fn file_logging_archive_lifecycle() -> anyhow::Result<()> {
    if let Ok(mode) = std::env::var(CHILD_MODE) {
        let directory =
            PathBuf::from(std::env::var_os(CHILD_DIR).ok_or_else(|| {
                anyhow::anyhow!("{CHILD_DIR} is required for file logging child")
            })?);
        return run_child_scenario(&mode, &directory);
    }

    let archive_dir = temporary_directory("archive");
    fs::create_dir_all(&archive_dir)?;
    for (hour, body) in [
        ("01", "fragment-one\n"),
        ("02", "fragment-two\n"),
        ("03", "fragment-three\n"),
    ] {
        write_old_gzip_fragment(
            &archive_dir.join(format!("latest.log.2020-01-01-{hour}.gz")),
            body,
        )?;
    }
    write_old_fragment(
        &archive_dir.join("latest.log.2019-12-31-23"),
        "raw-fragment\n",
    )?;

    // A prior process left a multi-member archive due for repacking.
    let repack_path = archive_dir.join("latest.log.2019-12-30.merged.gz");
    write_old_gzip_fragment(&repack_path, "first member\n")?;
    let append = fs::OpenOptions::new().append(true).open(&repack_path)?;
    let mut encoder = flate2::write::GzEncoder::new(append, flate2::Compression::default());
    encoder.write_all(b"second member\n")?;
    drop(encoder.finish()?);
    let repack_marker = archive_dir.join("latest.log.2019-12-30.merged.gz.needs-repack");
    write_old_fragment(&repack_marker, "needs repack\n")?;

    let output = run_child("archive", &archive_dir);
    assert_child_success(&output);
    assert!(
        !repack_marker.exists(),
        "startup must complete overdue repack"
    );
    let mut single_member = String::new();
    flate2::read::GzDecoder::new(File::open(&repack_path)?).read_to_string(&mut single_member)?;
    assert_eq!(single_member, "first member\nsecond member\n");
    assert_eq!(
        read_gzip(&archive_dir.join("latest.log.2020-01-01.merged.gz"))?,
        "fragment-one\nfragment-two\nfragment-three\n"
    );
    assert_eq!(
        read_gzip(&archive_dir.join("latest.log.2019-12-31-23.gz"))?,
        "raw-fragment\n"
    );

    let entries = fs::read_dir(&archive_dir)?.collect::<Result<Vec<_>, _>>()?;
    let names = entries
        .iter()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect::<Vec<_>>();
    assert!(
        names
            .iter()
            .any(|name| name == "latest.log.2020-01-01.merged.gz"),
        "expected same-day merged archive, found {names:?}"
    );
    assert!(
        names
            .iter()
            .all(|name| !name.starts_with("latest.log.2020-01-01-0")),
        "historical source fragments must be removed after a successful merge: {names:?}"
    );
    assert!(
        names
            .iter()
            .any(|name| name == "latest.log.2019-12-31-23.gz"),
        "an old plain fragment must be compressed during startup maintenance: {names:?}"
    );

    let active_names = names
        .iter()
        .filter(|name| *name == "latest.log")
        .collect::<Vec<_>>();
    assert!(
        active_names.len() == 1,
        "exactly one fixed active file must remain uncompressed: {names:?}"
    );
    assert_active_markers(&archive_dir.join("latest.log"), 1)?;

    // A second independent process exercises startup recovery and idempotency.
    let merged_before = fs::read(archive_dir.join("latest.log.2020-01-01.merged.gz"))?;
    assert_child_success(&run_child("archive", &archive_dir));
    assert_eq!(
        fs::read(archive_dir.join("latest.log.2020-01-01.merged.gz"))?,
        merged_before
    );
    assert_active_markers(&archive_dir.join("latest.log"), 2)?;

    let custom_dir = temporary_directory("custom");
    fs::create_dir_all(&custom_dir)?;
    write_old_fragment(
        &custom_dir.join("application.log.2020-01-01"),
        "custom prefix\n",
    )?;
    assert_child_success(&run_child("custom", &custom_dir));
    assert_eq!(
        read_gzip(&custom_dir.join("application.log.2020-01-01.gz"))?,
        "custom prefix\n"
    );
    assert!(!custom_dir.join("latest.log.2020-01-01.gz").exists());

    fs::remove_dir_all(&archive_dir)?;
    fs::remove_dir_all(&custom_dir)?;
    Ok(())
}

#[test]
fn fixed_active_file_rotates_on_startup_and_appends_on_same_period_restart() -> anyhow::Result<()> {
    let directory = temporary_directory("fixed-active");
    fs::create_dir_all(&directory)?;
    let active = directory.join("latest.log");
    write_historical_active(&active, "previous period\n")?;

    assert_child_success(&run_child("default", &directory));
    let archive = directory.join("latest.2020-01-01.log.gz");
    assert_eq!(read_gzip(&archive)?, "previous period\n");
    assert_active_markers(&active, 1)?;
    let archive_before = fs::read(&archive)?;
    let active_before = fs::read(&active)?;
    let names_before = fs::read_dir(&directory)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;

    assert_child_success(&run_child("default", &directory));
    assert_eq!(fs::read(&archive)?, archive_before);
    assert!(fs::read(&active)?.starts_with(&active_before));
    assert_active_markers(&active, 2)?;
    let names_after = fs::read_dir(&directory)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
    assert_eq!(
        names_after, names_before,
        "same-period restart must only append"
    );
    fs::remove_dir_all(directory)?;
    Ok(())
}

#[test]
fn new_archive_names_merge_repack_and_preserve_custom_active_names() -> anyhow::Result<()> {
    let directory = temporary_directory("new-archive-names");
    fs::create_dir_all(&directory)?;
    for hour in 1..=3 {
        write_old_gzip_fragment(
            &directory.join(format!("latest.2020-01-01-{hour:02}.log.gz")),
            &format!("fragment {hour}\n"),
        )?;
    }
    let plain = directory.join("latest.2019-12-31-23.log");
    write_old_fragment(&plain, "closed plain file\n")?;
    let repack = directory.join("latest.2019-12-30.merged.log.gz");
    write_old_gzip_fragment(&repack, "first member\n")?;
    let mut encoder = flate2::write::GzEncoder::new(
        OpenOptions::new().append(true).open(&repack)?,
        flate2::Compression::default(),
    );
    encoder.write_all(b"second member\n")?;
    drop(encoder.finish()?);
    let marker = directory.join("latest.2019-12-30.merged.log.gz.needs-repack");
    write_old_fragment(&marker, "needs repack\n")?;

    assert_child_success(&run_child("archive", &directory));
    assert_active_markers(&directory.join("latest.log"), 1)?;
    assert_eq!(
        read_gzip(&directory.join("latest.2020-01-01.merged.log.gz"))?,
        "fragment 1\nfragment 2\nfragment 3\n"
    );
    for hour in 1..=3 {
        assert!(
            !directory
                .join(format!("latest.2020-01-01-{hour:02}.log.gz"))
                .exists()
        );
    }
    assert!(!plain.exists());
    assert_eq!(
        read_gzip(&directory.join("latest.2019-12-31-23.log.gz"))?,
        "closed plain file\n"
    );
    assert!(!marker.exists());
    let mut single_member = String::new();
    flate2::read::GzDecoder::new(File::open(&repack)?).read_to_string(&mut single_member)?;
    assert_eq!(single_member, "first member\nsecond member\n");
    fs::remove_dir_all(directory)?;

    for (mode, active_name, archive_name) in [
        ("custom", "application.log", "application.2020-01-01.log.gz"),
        ("custom-bare", "app", "app.2020-01-01.gz"),
    ] {
        let directory = temporary_directory(mode);
        fs::create_dir_all(&directory)?;
        let active = directory.join(active_name);
        write_historical_active(&active, "custom previous period\n")?;
        assert_child_success(&run_child(mode, &directory));
        assert_eq!(
            read_gzip(&directory.join(archive_name))?,
            "custom previous period\n"
        );
        assert_active_markers(&active, 1)?;
        assert!(!directory.join("latest.log").exists());
        fs::remove_dir_all(directory)?;
    }
    Ok(())
}

#[test]
fn startup_rotation_preserves_existing_plain_and_gzip_archives() -> anyhow::Result<()> {
    let directory = temporary_directory("collision");
    fs::create_dir_all(&directory)?;
    let plain = directory.join("latest.2020-01-01.log");
    let compressed = directory.join("latest.2020-01-01.1.log.gz");
    fs::write(&plain, "original closed file\n")?;
    write_old_gzip_fragment(&compressed, "existing archive\n")?;
    let compressed_before = fs::read(&compressed)?;
    write_historical_active(&directory.join("latest.log"), "previous active file\n")?;

    assert_child_success(&run_child("default", &directory));
    assert_eq!(
        read_gzip(&directory.join("latest.2020-01-01.log.gz"))?,
        "original closed file\n"
    );
    assert_eq!(fs::read(compressed)?, compressed_before);
    assert_eq!(
        read_gzip(&directory.join("latest.2020-01-01.2.log.gz"))?,
        "previous active file\n"
    );
    assert_active_markers(&directory.join("latest.log"), 1)?;
    fs::remove_dir_all(directory)?;
    Ok(())
}

#[test]
fn file_logging_archive_failures_do_not_block_independent_candidates_or_restart()
-> anyhow::Result<()> {
    let directory = temporary_directory("independent-failures");
    fs::create_dir_all(&directory)?;

    let bad_plain = directory.join("latest.log.2019-12-20-00");
    let bad_plain_destination = directory.join("latest.log.2019-12-20-00.gz");
    write_old_fragment(&bad_plain, "plain source must survive\n")?;
    fs::write(&bad_plain_destination, b"invalid gzip destination\n")?;

    let bad_compaction_day = "2020-01-01";
    let bad_compaction_target =
        directory.join(format!("latest.log.{bad_compaction_day}.merged.gz"));
    fs::write(&bad_compaction_target, b"invalid merged destination\n")?;
    let mut bad_inputs = vec![
        bad_plain.clone(),
        bad_plain_destination,
        bad_compaction_target,
    ];
    for hour in 1..=3 {
        let fragment = directory.join(format!("latest.log.{bad_compaction_day}-{hour:02}.gz"));
        write_old_gzip_fragment(&fragment, &format!("preserve fragment {hour}\n"))?;
        bad_inputs.push(fragment);
    }

    let bad_repack_archive = directory.join("latest.log.2019-12-22.merged.gz");
    let bad_repack_marker = directory.join("latest.log.2019-12-22.merged.gz.needs-repack");
    fs::write(&bad_repack_archive, b"invalid repack archive\n")?;
    write_old_fragment(&bad_repack_marker, "needs repack\n")?;
    bad_inputs.extend([bad_repack_archive.clone(), bad_repack_marker]);
    let bad_input_contents = bad_inputs
        .iter()
        .map(|path| fs::read(path).map(|contents| (path.clone(), contents)))
        .collect::<std::io::Result<Vec<_>>>()?;

    let good_plain = directory.join("latest.log.2019-12-21-00");
    write_old_fragment(&good_plain, "independent plain source\n")?;
    for hour in 1..=3 {
        write_old_gzip_fragment(
            &directory.join(format!("latest.log.2020-01-02-{hour:02}.gz")),
            &format!("independent fragment {hour}\n"),
        )?;
    }
    let good_repack_archive = directory.join("latest.log.2019-12-23.merged.gz");
    write_old_gzip_fragment(&good_repack_archive, "first independent member\n")?;
    let append = fs::OpenOptions::new()
        .append(true)
        .open(&good_repack_archive)?;
    let mut encoder = flate2::write::GzEncoder::new(append, flate2::Compression::default());
    encoder.write_all(b"second independent member\n")?;
    drop(encoder.finish()?);
    let good_repack_marker = directory.join("latest.log.2019-12-23.merged.gz.needs-repack");
    write_old_fragment(&good_repack_marker, "needs repack\n")?;

    let assert_bad_inputs_preserved = || -> anyhow::Result<()> {
        for (path, contents) in &bad_input_contents {
            assert_eq!(
                fs::read(path)?,
                *contents,
                "must preserve {}",
                path.display()
            );
        }
        Ok(())
    };
    let assert_candidate_errors = |output: &Output| {
        let stderr = String::from_utf8_lossy(&output.stderr);
        for context in [
            format!("compressing {}", bad_plain.display()),
            format!("compacting {bad_compaction_day}"),
            format!("validating repack archive {}", bad_repack_archive.display()),
        ] {
            assert!(
                stderr.lines().any(|line| {
                    line.contains("log archive candidate failed") && line.contains(&context)
                }),
                "missing archive error context {context:?}:\n{stderr}"
            );
        }
    };

    let first = run_child("archive", &directory);
    assert_child_success(&first);
    assert_candidate_errors(&first);
    assert_bad_inputs_preserved()?;
    assert!(!good_plain.exists());
    assert_eq!(
        read_gzip(&directory.join("latest.log.2019-12-21-00.gz"))?,
        "independent plain source\n"
    );
    assert_eq!(
        read_gzip(&directory.join("latest.log.2020-01-02.merged.gz"))?,
        "independent fragment 1\nindependent fragment 2\nindependent fragment 3\n"
    );
    for hour in 1..=3 {
        assert!(
            !directory
                .join(format!("latest.log.2020-01-02-{hour:02}.gz"))
                .exists()
        );
    }
    assert!(!good_repack_marker.exists());
    let mut repacked = String::new();
    flate2::read::GzDecoder::new(File::open(&good_repack_archive)?)
        .read_to_string(&mut repacked)?;
    assert_eq!(
        repacked,
        "first independent member\nsecond independent member\n"
    );

    // The failed compaction leaves a transaction for startup recovery. A new
    // daemon must still process unrelated work while those inputs remain bad.
    let transaction_root = directory.join(".service-daemon-archive-transactions");
    let mut has_pending_manifest = false;
    for namespace in fs::read_dir(transaction_root)? {
        for entry in fs::read_dir(namespace?.path())? {
            if entry?
                .path()
                .extension()
                .is_some_and(|extension| extension == "manifest")
            {
                has_pending_manifest = true;
            }
        }
    }
    assert!(
        has_pending_manifest,
        "expected a failed compaction recovery record"
    );
    let after_restart = directory.join("latest.log.2019-12-24-00");
    write_old_fragment(&after_restart, "new work after restart\n")?;
    let second = run_child("archive", &directory);
    assert_child_success(&second);
    assert_candidate_errors(&second);
    assert_bad_inputs_preserved()?;
    assert!(!after_restart.exists());
    assert_eq!(
        read_gzip(&directory.join("latest.log.2019-12-24-00.gz"))?,
        "new work after restart\n"
    );

    fs::remove_dir_all(directory)?;
    Ok(())
}

#[test]
fn file_logging_none_and_never_leave_plain_files() -> anyhow::Result<()> {
    if let Ok(mode) = std::env::var(CHILD_MODE) {
        let directory =
            PathBuf::from(std::env::var_os(CHILD_DIR).ok_or_else(|| {
                anyhow::anyhow!("{CHILD_DIR} is required for file logging child")
            })?);
        return run_child_scenario(&mode, &directory);
    }

    let none_dir = temporary_directory("none");
    fs::create_dir_all(&none_dir)?;
    let plain_none = none_dir.join("custom.log.2020-01-01-01");
    write_old_fragment(&plain_none, "leave uncompressed\n")?;
    write_historical_active(
        &none_dir.join("custom.log"),
        "previous active stays plain\n",
    )?;
    let output = run_child("none", &none_dir);
    assert_child_success(&output);
    assert_eq!(fs::read_to_string(&plain_none)?, "leave uncompressed\n");
    assert_eq!(
        fs::read_to_string(none_dir.join("custom.2020-01-01-01.log"))?,
        "previous active stays plain\n"
    );
    assert_active_markers(&none_dir.join("custom.log"), 1)?;
    let none_names = fs::read_dir(&none_dir)?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect::<Vec<_>>();
    assert!(
        none_names
            .iter()
            .any(|name| name == "custom.2020-01-01-01.log"),
        "None compression must still rename the closed active file: {none_names:?}"
    );
    assert!(
        none_names.iter().all(|name| !name.ends_with(".gz")),
        "None compression must not create gzip archives: {none_names:?}"
    );

    let never_dir = temporary_directory("never");
    fs::create_dir_all(&never_dir)?;
    let plain_never = never_dir.join("never.log.2020-01-01");
    write_old_fragment(&plain_never, "leave historical files\n")?;
    write_historical_active(&never_dir.join("never.log"), "never rotate this prefix\n")?;
    let output = run_child("never", &never_dir);
    assert_child_success(&output);
    assert_eq!(
        fs::read_to_string(&plain_never)?,
        "leave historical files\n"
    );
    assert!(
        fs::read_to_string(never_dir.join("never.log"))?.starts_with("never rotate this prefix\n")
    );
    assert_active_markers(&never_dir.join("never.log"), 1)?;
    let never_names = fs::read_dir(&never_dir)?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect::<Vec<_>>();
    assert!(
        never_names.iter().any(|name| name == "never.log"),
        "Never rotation must retain the configured file name: {never_names:?}"
    );
    assert!(
        never_names.iter().all(|name| !name.ends_with(".gz")),
        "Never rotation must not create archives: {never_names:?}"
    );
    assert_eq!(
        never_names.len(),
        2,
        "Never must not create rotated or maintenance files"
    );

    fs::remove_dir_all(&none_dir)?;
    fs::remove_dir_all(&never_dir)?;
    Ok(())
}

fn run_child_scenario(mode: &str, directory: &Path) -> anyhow::Result<()> {
    let config = match mode {
        "default" => FileLogConfig {
            directory: directory.to_string_lossy().into_owned(),
            ..FileLogConfig::default()
        },
        "custom" => FileLogConfig::new(directory.to_string_lossy(), "application.log"),
        "custom-bare" => FileLogConfig::new(directory.to_string_lossy(), "app"),
        "archive" => FileLogConfig {
            directory: directory.to_string_lossy().into_owned(),
            file_prefix: "latest.log".to_string(),
            rotation: RotationPolicy::Hourly,
            compression: FileLogCompression::Gzip,
        },
        "none" => FileLogConfig {
            directory: directory.to_string_lossy().into_owned(),
            file_prefix: "custom.log".to_string(),
            rotation: RotationPolicy::Hourly,
            compression: FileLogCompression::None,
        },
        "never" => FileLogConfig {
            directory: directory.to_string_lossy().into_owned(),
            file_prefix: "never.log".to_string(),
            rotation: RotationPolicy::Never,
            compression: FileLogCompression::Gzip,
        },
        _ => anyhow::bail!("unknown file logging child mode {mode:?}"),
    };

    enable_file_logging(config);
    tracing_subscriber::registry()
        .with(DaemonLayer)
        .try_init()
        .ok();
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let daemon = ServiceDaemon::builder()
            .with_registry(Registry::builder().with_tag("__file_log__").build())
            .with_restart_policy(RestartPolicy::for_testing())
            .build();
        daemon.run().await;
        {
            let span = tracing::info_span!(
                "archive_identity",
                service_instance_id = SERVICE_INSTANCE_ID,
                source_service_instance_id = SOURCE_SERVICE_INSTANCE_ID,
                message_id = MESSAGE_ID,
                instance_seq = 42_u64,
            );
            let _entered = span.enter();
            tracing::info!(target: "file_logging.archive.integration", "archive_test_marker");
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        daemon.shutdown();
        daemon.wait().await?;
        Ok::<(), anyhow::Error>(())
    })
}
