//! Integration tests for the Logging example.

use std::path::{Path, PathBuf};
use std::time::Duration;

use service_daemon::{DaemonLayer, FileLogConfig, enable_file_logging};
use service_daemon::{Registry, RestartPolicy, ServiceDaemon};
use tracing_subscriber::prelude::*;

async fn wait_for_file(directory: &Path, prefix: &str) -> anyhow::Result<PathBuf> {
    for _ in 0..100 {
        if let Ok(entries) = std::fs::read_dir(directory) {
            for entry in entries.flatten() {
                let path = entry.path();
                let matches_prefix = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name == prefix);
                if matches_prefix {
                    return Ok(path);
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    anyhow::bail!("timed out waiting for a log file in {directory:?}")
}

async fn wait_for_marker(directory: &Path, prefix: &str, marker: &str) -> anyhow::Result<PathBuf> {
    for _ in 0..100 {
        if let Ok(entries) = std::fs::read_dir(directory) {
            for entry in entries.flatten() {
                let path = entry.path();
                let matches_prefix = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name == prefix);
                if matches_prefix
                    && std::fs::read_to_string(&path)
                        .map(|contents| contents.contains(marker))
                        .unwrap_or(false)
                {
                    return Ok(path);
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    anyhow::bail!("timed out waiting for {marker:?} in {directory:?}")
}

/// Verifies that the daemon can start with file logging enabled
/// and produces output in the configured directory.
#[tokio::test]
async fn test_file_logging_initialization() -> anyhow::Result<()> {
    // Use a temp directory to avoid polluting the project
    let temp_dir =
        std::env::temp_dir().join(format!("service-daemon-log-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir)?;

    enable_file_logging(FileLogConfig::new(
        temp_dir.to_string_lossy().into_owned(),
        "test-app",
    ));
    tracing_subscriber::registry()
        .with(DaemonLayer)
        .try_init()
        .ok();

    let daemon = ServiceDaemon::builder()
        .with_registry(Registry::builder().with_tag("__file_log__").build())
        .with_restart_policy(RestartPolicy::for_testing())
        .build();

    daemon.run().await;
    let _log_path = wait_for_file(&temp_dir, "test-app").await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    tracing::info!(target: "example_logging.integration", "file_logging_test_marker");

    let log_path = wait_for_marker(&temp_dir, "test-app", "file_logging_test_marker").await?;
    let contents = std::fs::read_to_string(&log_path)?;
    let marker_event = contents
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .find(|event| event["message"] == "file_logging_test_marker")
        .ok_or_else(|| anyhow::anyhow!("marker event was not found in {log_path:?}"))?;
    assert_eq!(marker_event["target"], "example_logging.integration");

    daemon.shutdown();
    daemon.wait().await?;

    // Cleanup
    let _ = std::fs::remove_dir_all(&temp_dir);

    Ok(())
}
