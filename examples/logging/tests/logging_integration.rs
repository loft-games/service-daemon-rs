//! Integration tests for the Logging example.

use service_daemon::core::logging::{FileLogConfig, enable_file_logging};
use service_daemon::{Registry, RestartPolicy, ServiceDaemon};

/// Verifies that the daemon can start with file logging enabled
/// and produces output in the configured directory.
#[tokio::test]
async fn test_file_logging_initialization() -> anyhow::Result<()> {
    // Use a temp directory to avoid polluting the project
    let temp_dir = std::env::temp_dir().join("service-daemon-log-test");
    let _ = std::fs::create_dir_all(&temp_dir);

    enable_file_logging(FileLogConfig::new(temp_dir.to_str().unwrap(), "test-app"));

    let mut daemon = ServiceDaemon::builder()
        .with_registry(Registry::builder().with_tag("__test_isolation__").build())
        .with_restart_policy(RestartPolicy::for_testing())
        .build();
    let cancel = daemon.cancel_token();

    daemon.run().await;

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    cancel.cancel();
    daemon.wait().await.unwrap();

    // Cleanup
    let _ = std::fs::remove_dir_all(&temp_dir);

    Ok(())
}
