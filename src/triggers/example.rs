use std::sync::Arc;

use crate::providers::trigger_providers::{CleanupSchedule, TaskQueue, UserNotifier, WorkerQueue};
use crate::providers::typed_providers::{DbUrl, Port};
use service_daemon::{allow_sync, trigger};

// --- Cron Trigger ---
// Uses the schedule string from CleanupSchedule provider
#[trigger(template = Cron, target = CleanupSchedule)]
pub async fn cleanup_trigger(
    port: std::sync::Arc<Port>, // Now supports qualified paths!
) -> anyhow::Result<()> {
    tracing::info!(">>> Cleanup Trigger [Cron] fired, port: {}", port);
    Ok(())
}

/// A trigger that demonstrates reading a snapshot of shared global state.
/// By declaring Arc<GlobalStats>, we get a zero-lock snapshot.
/// Even if writers are busy, this reader never blocks!
#[trigger(template = Cron, target = CleanupSchedule)]
pub async fn stats_viewer(
    stats: Arc<crate::providers::typed_providers::GlobalStats>,
) -> anyhow::Result<()> {
    // Note: We don't need .read().await here!
    // We already have a consistent snapshot in the Arc.
    tracing::info!(
        ">>> Stats Viewer [Snapshot] total: {}, last status: '{}'",
        stats.total_processed,
        stats.last_status
    );
    Ok(())
}

// --- Broadcast Queue Triggers ---
// TaskQueue is a BroadcastQueue - BOTH handlers receive every message!

#[trigger(template = Queue, target = TaskQueue)]
pub async fn worker_trigger(
    payload: String,
    port: Arc<Port>,
    db_url: Arc<DbUrl>,
) -> anyhow::Result<()> {
    tracing::info!(
        ">>> Worker Trigger 1 [Broadcast] received: '{}', port: {}, db_url: {}",
        payload,
        port,
        db_url
    );
    Ok(())
}

#[trigger(template = BQueue, target = TaskQueue)]
pub async fn worker_trigger2(payload: String, port: Arc<Port>) -> anyhow::Result<()> {
    tracing::info!(
        ">>> Worker Trigger 2 [Broadcast] received: '{}', port: {}",
        payload,
        port
    );
    Ok(())
}

// --- Load-Balancing Queue Trigger ---
// WorkerQueue is an LBQueue - messages are distributed to one handler at a time
#[trigger(template = LBQueue, target = WorkerQueue)]
pub async fn lb_worker_trigger(payload: String, port: Arc<Port>) -> anyhow::Result<()> {
    tracing::info!(
        ">>> LB Worker Trigger [LoadBalancing] received: '{}', port: {}",
        payload,
        port
    );
    Ok(())
}

// --- Complex Payload with Explicit Arc ---
// Using #[payload] allows receiving the event payload wrapped in Arc
#[trigger(template = LBQueue, target = crate::providers::trigger_providers::JobQueue)]
pub async fn complex_job_handler(
    #[payload] job: Arc<crate::providers::trigger_providers::ComplexJob>,
    port: Arc<Port>,
) -> anyhow::Result<()> {
    tracing::info!(
        ">>> Complex Job Handler received Arc payload: id={}, data='{}', port: {}",
        job.id,
        job.data,
        port
    );
    Ok(())
}

// --- Signal Trigger ---
// Uses the Notify provider for event signaling
#[trigger(template = Event, target = UserNotifier)]
pub async fn notify_trigger(port: Arc<Port>) -> anyhow::Result<()> {
    tracing::info!(">>> Notify Trigger [Event] received, port: {}", port);
    Ok(())
}

// --- Sync Trigger ---
#[allow_sync]
#[trigger(template = Notify, target = UserNotifier)]
pub fn sync_notify_trigger() -> anyhow::Result<()> {
    tracing::info!(">>> Sync Notify Trigger fired");
    Ok(())
}

// --- Watch Trigger ---
// Fires whenever GlobalStats is modified (via Arc<RwLock<GlobalStats>>)
#[trigger(template = Watch, target = crate::providers::typed_providers::GlobalStats)]
pub async fn on_stats_changed(
    snapshot: Arc<crate::providers::typed_providers::GlobalStats>,
) -> anyhow::Result<()> {
    tracing::info!(
        ">>> Watch Trigger [State Change] detected update: total={}, last='{}'",
        snapshot.total_processed,
        snapshot.last_status
    );
    Ok(())
}
