//! Runnable cross-platform local IPC provider example.
//!
//! Demonstrates:
//! - `LocalIpcListen` for the server side.
//! - `LocalIpcConnect` for the client side.
//! - A shared `AsyncRead + AsyncWrite` request/response flow over the platform
//!   local IPC stream.

use example_local_ipc as _;
use service_daemon::{ServiceDaemon, ServiceError};
use tracing::{error, info};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    service_daemon::init_logging();

    let daemon = ServiceDaemon::builder().build();
    daemon.run().await;

    match daemon.wait().await {
        Ok(()) => {
            info!("Local IPC example daemon stopped cleanly");
            Ok(())
        }
        Err(ServiceError::InternalError(message)) => {
            error!(
                reason = %message,
                "Local IPC example could not install an OS shutdown signal listener"
            );
            Err(ServiceError::InternalError(message).into())
        }
        Err(error) => {
            error!(
                %error,
                "Local IPC example daemon wait failed outside the documented signal-listener path"
            );
            Err(error.into())
        }
    }
}
