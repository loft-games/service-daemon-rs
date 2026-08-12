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
use service_daemon::{FileLogConfig, ServiceDaemon, ServiceError, enable_file_logging};
use tracing::{error, info};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    service_daemon::init_logging();

    // Enable file-based JSON log persistence (consumed by file_log_service)
    enable_file_logging(FileLogConfig::new("logs", "my-app"));

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
