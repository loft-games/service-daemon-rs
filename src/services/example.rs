use crate::providers::trigger_providers::{TaskQueue, UserNotifier};
use crate::providers::typed_providers::{DbUrl, GlobalStats, Port};
use service_daemon::prelude::*;
use service_daemon::{allow_sync, service};
use tracing::info;

#[service]
pub async fn example_service(
    port: std::sync::Arc<Port>, // Now supports qualified paths!
    db_url: Arc<DbUrl>,
) -> anyhow::Result<()> {
    // TIP: Hover over 'Port' or 'DbUrl' above - doc hints are preserved!
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

// --- External Modification Mock Service ---
// This service simulates an "external" callback or context that doesn't
// have the state injected as a parameter, but uses the new static methods.
#[service]
pub async fn external_status_updater() -> anyhow::Result<()> {
    info!("External status updater mock service started");
    let mut count = 0;

    while !service_daemon::is_shutdown() {
        // Wait 15 seconds between updates
        tokio::select! {
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(15)) => {
                count += 1;
                let msg = format!("External update #{}", count);

                // DIRECT MODIFICATION SYNTAX (Zero Overhead)
                // We obtain a genuine TrackedRwLock object directly.
                let state_lock = crate::providers::trigger_providers::ExternalStatus::rwlock().await;
                {
                    let mut guard = state_lock.write().await;
                    guard.message = msg;
                    guard.updated_count = count;
                    info!("Pushed external update #{}", count);
                } // <--- Trigger fires here when guard is dropped!
            }
            _ = service_daemon::wait_for_shutdown() => break,
        }
    }
    Ok(())
}

// --- Priority Verification Examples ---

/// A service with SYSTEM priority (100).
/// This will be the FIRST to start and the LAST to shut down.
#[service(priority = ServicePriority::SYSTEM)]
pub async fn log_flusher() -> anyhow::Result<()> {
    info!("[SYSTEM] Log flusher started (Priority 100)");
    service_daemon::wait_for_shutdown().await;
    info!("[SYSTEM] Log flusher exiting LAST");
    Ok(())
}

/// A service with STORAGE priority (80).
/// Starts after SYSTEM, shuts down before SYSTEM.
#[service(priority = ServicePriority::STORAGE)]
pub async fn db_connector(db_url: Arc<DbUrl>) -> anyhow::Result<()> {
    info!(
        "[STORAGE] DB connector started (Priority 80) for {}",
        db_url
    );
    service_daemon::wait_for_shutdown().await;
    info!("[STORAGE] DB connector exiting");
    Ok(())
}

/// A service with EXTERNAL priority (0).
/// This will be the LAST to start and the FIRST to shut down.
#[service(priority = ServicePriority::EXTERNAL)]
pub async fn public_api(port: Arc<Port>) -> anyhow::Result<()> {
    info!(
        "[EXTERNAL] Public API started (Priority 0) on port {}",
        port
    );
    service_daemon::wait_for_shutdown().await;
    info!("[EXTERNAL] Public API exiting FIRST");
    Ok(())
}
