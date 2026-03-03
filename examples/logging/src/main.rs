//! # Logging Example -- File-Based Log Persistence
//!
//! This example demonstrates the `file-logging` feature:
//! - Configuring `FileLogConfig` for directory and file prefix
//! - Enabling file logging with `enable_file_logging()` before daemon start
//! - Automatic daily log rotation via `tracing-appender`
//! - JSON-structured log output
//!
//! **Run**: `cargo run -p example-logging`
//!
//! After running, check the `logs/` directory for files named
//! `my-app.YYYY-MM-DD` containing JSON-structured log lines.

use example_logging as _;
use service_daemon::ServiceDaemon;
use service_daemon::core::logging::{FileLogConfig, enable_file_logging};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    service_daemon::core::logging::init_logging();

    // Enable file-based JSON log persistence (consumed by file_log_service)
    enable_file_logging(FileLogConfig::new("logs", "my-app"));

    let mut daemon = ServiceDaemon::builder().build();
    daemon.run().await;
    daemon.wait().await?;

    Ok(())
}
