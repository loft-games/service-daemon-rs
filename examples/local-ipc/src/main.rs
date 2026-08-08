//! Runnable cross-platform local IPC provider example.
//!
//! Demonstrates:
//! - `LocalIpcListen` for the server side.
//! - `LocalIpcConnect` for the client side.
//! - A shared `AsyncRead + AsyncWrite` request/response flow over the platform
//!   local IPC stream.

use service_daemon::ServiceDaemon;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let mut daemon = ServiceDaemon::builder().build();
    daemon.run().await;
    daemon.wait().await?;
    Ok(())
}
