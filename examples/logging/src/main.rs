//! # File logging
//!
//! Run with `cargo run -p example-logging`, then read `logs/latest.log`.
//! Events are written as JSON lines. Default rotation is 8h ±5% or 100MiB;
//! background gzip builds fragment, daily and monthly archives.
//! See `docs/guide/diagnostics.md` for archive settings and recovery behavior.

use example_logging as _;
use service_daemon::{FileLogConfig, ServiceDaemon, ServiceError, try_enable_file_logging};
use tracing::{error, info};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    service_daemon::init_logging();

    try_enable_file_logging(FileLogConfig::default())?;

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
