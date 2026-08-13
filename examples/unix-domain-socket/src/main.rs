//! # Unix domain socket example
//!
//! This example demonstrates the Unix domain socket provider templates:
//! - `UnixListen` for the server side.
//! - `UnixConnect` for the client side.
//! - A `ping` / `pong` exchange over the generated `accept()` / `connect()`
//!   stream helpers.
//!
//! **Run**: `cargo run -p example-unix-domain-socket`

#[cfg(unix)]
use example_unix_domain_socket as _;
#[cfg(unix)]
use example_unix_domain_socket::providers::EXAMPLE_UNIX_DOMAIN_SOCKET_PATH;
#[cfg(unix)]
use service_daemon::{ServiceDaemon, ServiceError};
#[cfg(unix)]
use tracing::{error, info};

#[cfg(unix)]
fn cleanup_socket_path() -> anyhow::Result<()> {
    match std::fs::remove_file(EXAMPLE_UNIX_DOMAIN_SOCKET_PATH) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

#[cfg(unix)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    service_daemon::init_logging();
    cleanup_socket_path()?;

    let daemon = ServiceDaemon::builder().build();
    daemon.run().await;

    let wait_result = daemon.wait().await;
    cleanup_socket_path()?;

    match wait_result {
        Ok(()) => {
            info!("Unix domain socket example daemon stopped cleanly");
            Ok(())
        }
        Err(ServiceError::InternalError(message)) => {
            error!(
                reason = %message,
                "Unix domain socket example could not install an OS shutdown signal listener"
            );
            Err(ServiceError::InternalError(message).into())
        }
        Err(error) => {
            error!(
                %error,
                "Unix domain socket example daemon wait failed outside the documented signal-listener path"
            );
            Err(error.into())
        }
    }
}

#[cfg(not(unix))]
fn main() {
    service_daemon::init_logging();
    tracing::warn!("The Unix domain socket example only runs on Unix targets.");
}
