//! # On-Demand Service Instance Example
//!
//! This example demonstrates:
//! - Marking a service definition with `#[service(auto_start = false)]`
//! - Resolving a daemon-bound `ServiceHandle` from a provider
//! - Creating runtime instances with `ServiceHandle::create().await`
//! - Starting created instances with `ServiceInstanceHandle::start().await`
//! - Stopping and cleaning instances with `ServiceInstanceHandle`
//!
//! **Run**: `RUST_LOG=info cargo run -p example-on-demand`

use example_on_demand as _;
use service_daemon::{Registry, ServiceDaemon, ServiceError};
use tracing::{error, info};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    service_daemon::init_logging();

    let daemon = ServiceDaemon::builder()
        .with_registry(Registry::builder().with_tag("on-demand").build())
        .build();
    daemon.run().await;

    match daemon.wait().await {
        Ok(()) => {
            info!("On-demand example daemon stopped cleanly");
            Ok(())
        }
        Err(ServiceError::InternalError(message)) => {
            error!(
                reason = %message,
                "On-demand example could not install an OS shutdown signal listener"
            );
            Err(ServiceError::InternalError(message).into())
        }
        Err(error) => {
            error!(
                %error,
                "On-demand example daemon wait failed outside the documented signal-listener path"
            );
            Err(error.into())
        }
    }
}
