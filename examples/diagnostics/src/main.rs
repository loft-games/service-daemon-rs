use example_diagnostics as _;

use service_daemon::{ServiceDaemon, ServiceError};
use tracing::{error, info};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    service_daemon::init_logging();

    let daemon = ServiceDaemon::builder().build();

    daemon.run().await;

    info!("Daemon running. Press Ctrl+C to stop and export the graph.");

    // Wait for the daemon to stop (handles Ctrl+C/SIGINT and auto-exports topology internally).
    match daemon.wait().await {
        Ok(()) => {
            info!("Diagnostics example daemon stopped cleanly");
            Ok(())
        }
        Err(ServiceError::InternalError(message)) => {
            error!(
                reason = %message,
                "Diagnostics example could not install an OS shutdown signal listener"
            );
            Err(ServiceError::InternalError(message).into())
        }
        Err(error) => {
            error!(
                %error,
                "Diagnostics example daemon wait failed outside the documented signal-listener path"
            );
            Err(error.into())
        }
    }
}
