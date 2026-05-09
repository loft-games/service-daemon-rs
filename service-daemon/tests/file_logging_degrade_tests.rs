#![cfg(feature = "file-logging")]

use std::time::Duration;

use service_daemon::{FileLogConfig, Registry, RestartPolicy, ServiceDaemon, enable_file_logging};

#[tokio::test]
async fn file_logging_appender_init_failure_degrades_to_console_only() -> anyhow::Result<()> {
    let invalid_dir = std::env::temp_dir().join(format!(
        "service-daemon-file-logging-invalid-dir-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&invalid_dir);
    let _ = std::fs::remove_dir_all(&invalid_dir);
    std::fs::write(&invalid_dir, b"not a directory")?;

    enable_file_logging(FileLogConfig::new(
        invalid_dir.to_string_lossy().into_owned(),
        "test-app",
    ));

    let mut daemon = ServiceDaemon::builder()
        .with_registry(Registry::builder().with_tag("__file_log__").build())
        .with_restart_policy(RestartPolicy::for_testing())
        .build();
    let cancel = daemon.cancel_token();

    daemon.run().await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), daemon.wait()).await??;

    let _ = std::fs::remove_file(&invalid_dir);
    Ok(())
}
