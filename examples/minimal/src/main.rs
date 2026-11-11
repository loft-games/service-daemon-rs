//! # Minimal Example -- `is_shutdown()` Polling Pattern
//!
//! This is the simplest way to use `service-daemon`. It demonstrates:
//! - Defining a service with `#[service]`
//! - Using `is_shutdown()` for graceful exit
//! - Basic dependency injection via `#[provider]`
//! - The interruptible `sleep()` helper
//!
//! **Run**: `cargo run -p example-minimal`
//!
//! > [!WARNING]
//! > Do NOT mix `is_shutdown()` polling with `state()` lifecycle matching
//! > in the same service. These are two independent control-flow paradigms;
//! > mixing them leads to undefined behavior.

use example_minimal as _;
use service_daemon::{ServiceDaemon, ServiceError};
use tracing::{error, info};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    service_daemon::init_logging();

    let daemon = ServiceDaemon::builder().build();
    daemon.run().await;

    match daemon.wait().await {
        Ok(()) => {
            info!("Minimal example daemon stopped cleanly");
            Ok(())
        }
        Err(ServiceError::InternalError(message)) => {
            error!(
                reason = %message,
                "Minimal example could not install an OS shutdown signal listener"
            );
            Err(ServiceError::InternalError(message).into())
        }
        Err(error) => {
            error!(
                %error,
                "Minimal example daemon wait failed outside the documented signal-listener path"
            );
            Err(error.into())
        }
    }
}
