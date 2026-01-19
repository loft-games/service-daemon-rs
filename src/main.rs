mod providers;
mod services;

use service_daemon::ServiceDaemon;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Setup Logging
    tracing_subscriber::fmt::init();

    // 2. Initialize Daemon (auto-runs providers, then registers services)
    let daemon = ServiceDaemon::auto_init();

    // 3. Run Daemon
    daemon.run().await?;

    Ok(())
}
