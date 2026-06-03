//! # Controller Bridge Example -- Simulated Connection Layer Shape
//!
//! This example demonstrates a controller-style topology:
//! - a provider owns a deterministic in-memory connection handle,
//! - the adapter layer performs transport, framing, protobuf, reconnect, and correlation work,
//! - a custom `TriggerHost` turns connection events into trigger transitions,
//! - thin trigger handlers delegate stats and status side effects to services.
//!
//! **Run**: `cargo run -p example-controller-bridge`

use example_controller_bridge as _;
use service_daemon::{ServiceDaemon, ServiceError};
use tracing::{error, info};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    service_daemon::init_logging();

    let mut daemon = ServiceDaemon::builder().build();
    daemon.run().await;

    match daemon.wait().await {
        Ok(()) => {
            info!("Controller bridge example daemon stopped cleanly");
            Ok(())
        }
        Err(ServiceError::InternalError(message)) => {
            error!(
                reason = %message,
                "Controller bridge example could not install an OS shutdown signal listener"
            );
            Err(ServiceError::InternalError(message).into())
        }
        Err(error) => {
            error!(
                %error,
                "Controller bridge example daemon wait failed outside the documented signal-listener path"
            );
            Err(error.into())
        }
    }
}
