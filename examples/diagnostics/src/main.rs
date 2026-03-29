use example_diagnostics as _;

use service_daemon::ServiceDaemon;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    service_daemon::core::logging::init_logging();

    let mut daemon = ServiceDaemon::builder().build();

    daemon.run().await;

    info!("Daemon running. Press Ctrl+C to stop and export the graph.");

    // Wait for the daemon to stop (handles Ctrl+C/SIGINT and auto-exports topology internally)
    let _ = daemon.wait().await;

    Ok(())
}
