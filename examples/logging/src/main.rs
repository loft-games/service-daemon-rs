//! # Logging Example -- File-Based Log Persistence
//!
//! This example demonstrates the `file-logging` feature:
//! - Configuring `FileLogConfig` for directory, file prefix, and compression
//! - Enabling file logging with `enable_file_logging()` before daemon start
//! - Automatic daily log rotation and background gzip archiving
//! - JSON-structured log output
//!
//! **Run**: `cargo run -p example-logging`
//!
//! While running or after shutdown, read `logs/latest.log` for JSON log lines.
//! The first write after a UTC day boundary rotates the previous file to
//! `latest.YYYY-MM-DD.log`, then compresses it as `latest.YYYY-MM-DD.log.gz`.

use example_logging as _;
use service_daemon::{FileLogConfig, ServiceDaemon, ServiceError, enable_file_logging};
use tracing::{error, info};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    service_daemon::init_logging();

    // Enable file-based JSON log persistence in the active latest.log file
    // and background gzip archiving (consumed by file_log_service).
    enable_file_logging(FileLogConfig::default());

    let daemon = ServiceDaemon::builder().build();
    daemon.run().await;

    match daemon.wait().await {
        Ok(()) => {
            info!("Logging example daemon stopped cleanly");
            Ok(())
        }
        Err(ServiceError::InternalError(message)) => {
            error!(
                reason = %message,
                "Logging example could not install an OS shutdown signal listener"
            );
            Err(ServiceError::InternalError(message).into())
        }
        Err(error) => {
            error!(
                %error,
                "Logging example daemon wait failed outside the documented signal-listener path"
            );
            Err(error.into())
        }
    }
}
