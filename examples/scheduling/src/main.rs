use examples_scheduling as _;

use anyhow::Result;
use service_daemon::{ServiceDaemon, ServiceError};
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter("info,service_daemon=info,examples_scheduling=info")
        .init();

    info!("Starting Priority & Scheduling Demo...");

    let daemon = ServiceDaemon::builder().build();

    daemon.run().await;

    match daemon.wait().await {
        Ok(()) => {
            info!("Scheduling example daemon stopped cleanly");
            Ok(())
        }
        Err(ServiceError::InternalError(message)) => {
            error!(
                reason = %message,
                "Scheduling example could not install an OS shutdown signal listener"
            );
            Err(ServiceError::InternalError(message).into())
        }
        Err(error) => {
            error!(
                %error,
                "Scheduling example daemon wait failed outside the documented signal-listener path"
            );
            Err(error.into())
        }
    }
}
