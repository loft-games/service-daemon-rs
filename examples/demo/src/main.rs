mod integration_tests;
mod providers;
mod services;
mod triggers;

use crate::providers::trigger_providers::{AsyncConfig, SyncConfig, UserNotifier};
use service_daemon::{Provided, RestartPolicy, ServiceDaemon};
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Setup Logging (including the new Non-blocking DaemonLayer)
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(service_daemon::utils::logging::DaemonLayer)
        .init();

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

    // 5. For demonstration: Push various jobs and messages
    tokio::spawn(async move {
        let mut job_id = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;

            // Push to WorkerQueue (LBQueue String)
            let _ = crate::providers::trigger_providers::WorkerQueue::push(format!(
                "LB Work Item #{}",
                job_id
            ))
            .await;

            // Push to JobQueue (LBQueue ComplexJob - demonstrates #[payload] Arc support)
            let _ = crate::providers::trigger_providers::JobQueue::push(
                crate::providers::trigger_providers::ComplexJob {
                    id: job_id,
                    data: format!("Complex Data for job {}", job_id),
                },
            )
            .await;

            job_id += 1;
        }
    });

    // 6. For demonstration: Query service status every 20 seconds
    let daemon_ref = daemon.handle();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(25)).await;
            let status = daemon_ref.get_service_status("example_service").await;
            tracing::info!("--- [Main] example_service status: {:?} ---", status);
        }
    });

    // 7. For demonstration: Log custom providers every 40 seconds
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(40)).await;
            let async_cfg = AsyncConfig::resolve().await;
            let sync_cfg = SyncConfig::resolve().await;
            tracing::info!(
                "--- [Main] AsyncConfig initialized at: {:?} ---",
                async_cfg.initialized_at
            );
            tracing::info!("--- [Main] SyncConfig value: {} ---", sync_cfg.value);
        }
    });

    // 7. Run Daemon (handles Ctrl+C / SIGTERM gracefully)
    daemon.run().await?;

    Ok(())
}
