mod providers;
mod services;
mod triggers;

use crate::providers::trigger_providers::UserNotifier;
use service_daemon::{RestartPolicy, ServiceDaemon};
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Setup Logging
    tracing_subscriber::fmt::init();

    // 2. Configure custom restart policy (optional)
    let policy = RestartPolicy::builder()
        .initial_delay(Duration::from_secs(2))
        .max_delay(Duration::from_secs(120)) // 2 minutes max
        .multiplier(1.5)
        .build();

    // 3. Initialize Daemon with custom policy
    let daemon = ServiceDaemon::from_registry_with_policy(policy);

    // 4. For demonstration: Spawn a task to fire the event notifier every 15 seconds
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            tracing::info!("--- Firing event notifier from main ---");
            UserNotifier::notify().await;
        }
    });

    // 5. Run Daemon (handles Ctrl+C / SIGTERM gracefully)
    daemon.run().await?;

    Ok(())
}
