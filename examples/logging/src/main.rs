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

use example_logging as _;
use service_daemon::ServiceDaemon;
use service_daemon::core::logging::{FileLogConfig, enable_file_logging};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(service_daemon::core::logging::DaemonLayer)
        .init();

    enable_file_logging(FileLogConfig::new("logs", "my-app"));

    let mut daemon = ServiceDaemon::builder().build();
    daemon.run().await;
    daemon.wait().await?;

    Ok(())
}
