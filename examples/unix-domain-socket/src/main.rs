//! # Unix domain socket example
//!
//! This example demonstrates the Unix domain socket provider templates:
//! - `UnixListen` for the server side.
//! - `UnixConnect` for the client side.
//! - The initialization probe connection opened by `UnixConnect`.
//! - A separate business connection opened through `connect().await?`.
//!
//! **Run**: `cargo run -p example-unix-domain-socket`

#[cfg(unix)]
use example_unix_domain_socket::providers::EXAMPLE_UNIX_DOMAIN_SOCKET_PATH;
#[cfg(unix)]
use service_daemon::ServiceDaemon;
#[cfg(unix)]
use std::time::Duration;

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
    use example_unix_domain_socket as _;

    service_daemon::init_logging();
    cleanup_socket_path()?;

    let mut daemon = ServiceDaemon::builder().build();
    daemon.run().await;

    tokio::time::sleep(Duration::from_secs(1)).await;
    daemon.shutdown();
    daemon.wait().await?;

    cleanup_socket_path()?;
    Ok(())
}

#[cfg(not(unix))]
fn main() {
    println!("The Unix domain socket example only runs on Unix targets.");
}
