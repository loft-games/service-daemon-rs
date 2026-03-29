use examples_scheduling as _;

use anyhow::Result;
use service_daemon::ServiceDaemon;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter("info,service_daemon=info,examples_scheduling=info")
        .init();

    info!("Starting Scheduling & Isolation Demo...");

    let mut daemon = ServiceDaemon::builder().build();

    daemon.run().await;
    daemon.wait().await?;

    Ok(())
}
