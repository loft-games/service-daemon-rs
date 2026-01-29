use crate::providers::trigger_providers::{TaskQueue, UserNotifier};
use crate::providers::typed_providers::{DbUrl, GlobalStats, Port};
use service_daemon::{allow_sync, service};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

#[service]
pub async fn example_service(port: Arc<Port>, db_url: Arc<DbUrl>) -> anyhow::Result<()> {
    // No .0 needed - Display is auto-generated!
    info!(
        "Example service running on port {} with DB {}",
        port, db_url
    );
    loop {
        // Use select to allow immediate cancellation during sleep
        tokio::select! {
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(10)) => {
                info!("Heartbeat from example service");

                // --- Template-Based Interaction ---

                // 1. Trigger a Signal (Event) from here
                UserNotifier::notify().await;

                // 2. Push to a Broadcast Queue
                let _ = TaskQueue::push("Message from service".to_owned()).await;
            }
            _ = service_daemon::wait_for_shutdown() => {
                info!("Example service shutting down");
                break;
            }
        }
    }
    Ok(())
}

#[service]
pub async fn stats_writer(stats: Arc<RwLock<GlobalStats>>) -> anyhow::Result<()> {
    info!("Stats writer service started");
    while !service_daemon::is_shutdown() {
        // Gain exclusive write access
        let mut guard = stats.write().await;
        guard.total_processed += 1;
        guard.last_status = format!("Processed {}", guard.total_processed);

        info!("Updated global stats: {}", guard.total_processed);

        // Wait or check for shutdown again
        tokio::select! {
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {}
            _ = service_daemon::wait_for_shutdown() => break,
        }
    }
    info!("Stats writer shutting down");
    Ok(())
}

#[allow_sync]
#[service]
pub fn sync_service(port: Arc<Port>) -> anyhow::Result<()> {
    // This is a synchronous service. It still works because the macro wraps it!
    // Note: Calling blocking code here would block the executor.
    info!("Sync service started on port {}", port);
    Ok(())
}
