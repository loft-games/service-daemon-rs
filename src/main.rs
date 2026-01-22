mod providers;
mod services;
mod triggers;

use service_daemon::ServiceDaemon;
use tokio::sync::Notify;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Setup Logging
    tracing_subscriber::fmt::init();

    // 2. Verify dependencies (only when 'macros' feature is enabled)
    service_daemon::verify_setup!();

    // 3. Initialize Daemon (auto-runs providers, then registers services/triggers)
    let daemon = ServiceDaemon::auto_init();

    // 4. For demonstration: Spawn a task to fire the custom notifier every 15 seconds
    if let Some(notifier) = service_daemon::GLOBAL_CONTAINER.resolve::<Notify>("user_notifier") {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                tracing::info!("--- Firing custom notifier from main ---");
                notifier.notify_one();
            }
        });
    }

    // 5. Run Daemon
    daemon.run().await?;

    Ok(())
}
