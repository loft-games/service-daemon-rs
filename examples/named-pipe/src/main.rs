//! # Windows named pipe example
//!
//! This example demonstrates the Windows named pipe provider templates:
//! - `NamedPipeListen` for the server side.
//! - `NamedPipeConnect` for the client side.
//! - The initialization probe connection opened by `NamedPipeConnect`.
//! - A separate business connection opened through `connect().await?`.
//!
//! **Run**: `cargo run -p example-named-pipe` on Windows.

#[cfg(windows)]
use example_named_pipe as _;
#[cfg(windows)]
use service_daemon::ServiceDaemon;

#[cfg(windows)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    service_daemon::init_logging();

    let daemon = ServiceDaemon::builder().build();
    daemon.run().await;
    daemon.wait().await?;

    Ok(())
}

#[cfg(not(windows))]
fn main() {
    service_daemon::init_logging();
    tracing::warn!("The Windows named pipe example only runs on Windows targets.");
}
