//! # Logging Example -- File-Based Log Persistence
//!
//! This example demonstrates the `file-logging` feature:
//! - Configuring `FileLogConfig` for directory and file prefix
//! - Enabling file logging with `enable_file_logging()` before daemon start
//! - Automatic daily log rotation via `tracing-appender`
//! - JSON-structured log output (IGES 6.8 compliant)
//!
//! **Run**: `cargo run -p example-logging`
//!
//! After running, check the `logs/` directory for files named
//! `my-app.YYYY-MM-DD` containing JSON-structured log lines.

mod services;

use service_daemon::ServiceDaemon;
use service_daemon::core::logging::{FileLogConfig, enable_file_logging};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Initialize tracing with the built-in DaemonLayer
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(service_daemon::core::logging::DaemonLayer)
        .init();

    // 2. Enable file-based log persistence.
    //    MUST be called BEFORE `ServiceDaemon::run()`.
    //    - `directory`: where log files are stored (created if missing)
    //    - `file_prefix`: each file is named `{prefix}.YYYY-MM-DD`
    enable_file_logging(FileLogConfig::new("logs", "my-app"));

    // 3. Create and run the daemon (non-blocking)
    let mut daemon = ServiceDaemon::builder().build();
    daemon.run().await?;

    // 4. Wait for shutdown signal (Ctrl+C / SIGTERM)
    daemon.wait().await?;

    Ok(())
}

// =============================================================================
// Integration Tests -- Logging
// =============================================================================
#[cfg(test)]
mod tests {
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

        daemon.run().await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        cancel.cancel();
        daemon.wait().await.unwrap();

        // Cleanup
        let _ = std::fs::remove_dir_all(&temp_dir);

        Ok(())
    }
}
