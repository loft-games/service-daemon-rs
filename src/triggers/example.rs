use std::sync::Arc;

use crate::providers::trigger_providers::{CleanupSchedule, TaskQueue, UserNotifier, WorkerQueue};
use crate::providers::typed_providers::{DbUrl, Port};
use service_daemon::{allow_sync, trigger};

// --- Cron Trigger ---
// Uses the schedule string from CleanupSchedule provider
#[trigger(template = Cron, target = CleanupSchedule)]
pub async fn cleanup_trigger(port: Arc<Port>) -> anyhow::Result<()> {
    tracing::info!(">>> Cleanup Trigger [Cron] fired, port: {}", port);
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
