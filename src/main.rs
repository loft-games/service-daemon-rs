mod providers;
mod services;
mod triggers;

use crate::providers::trigger_providers::UserNotifier;
use service_daemon::ServiceDaemon;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Setup Logging
    tracing_subscriber::fmt::init();

    // 2. Initialize Daemon (auto-runs providers, then registers services/triggers)
    let daemon = ServiceDaemon::auto_init();

    // 3. For demonstration: Spawn a task to fire the custom notifier every 15 seconds
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
            tracing::info!("--- Firing custom notifier from main ---");
            UserNotifier::notify();
        }
    });

    // 4. Run Daemon
    daemon.run().await?;

    Ok(())
}
