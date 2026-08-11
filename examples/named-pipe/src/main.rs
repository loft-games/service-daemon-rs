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
use service_daemon::ServiceDaemon;
#[cfg(windows)]
use std::time::Duration;

#[cfg(windows)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use example_named_pipe as _;

    service_daemon::init_logging();

    let daemon = ServiceDaemon::builder().build();
    daemon.run().await;

    tokio::time::sleep(Duration::from_secs(1)).await;
    daemon.shutdown();
    daemon.wait().await?;
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    println!("The Windows named pipe example only runs on Windows targets.");
}
