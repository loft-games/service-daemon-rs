use std::sync::Arc;

use crate::providers::trigger_providers::{CleanupSchedule, TaskQueue, UserNotifier, WorkerQueue};
use crate::providers::typed_providers::{DbUrl, Port};
use service_daemon::trigger;

// --- Cron Trigger ---
// Uses the schedule string from CleanupSchedule provider
#[trigger(template = "cron", target = CleanupSchedule)]
pub async fn cleanup_trigger(_payload: (), id: String, port: Arc<Port>) -> anyhow::Result<()> {
    tracing::info!(
        ">>> Cleanup Trigger [Cron] fired (id: {}), port: {}",
        id,
        port
    );
    Ok(())
}

// --- Broadcast Queue Triggers ---
// TaskQueue is a BroadcastQueue - BOTH handlers receive every message!

#[trigger(template = "queue", target = TaskQueue)]
pub async fn worker_trigger(
    payload: String,
    id: String,
    port: Arc<Port>,
    db_url: Arc<DbUrl>,
) -> anyhow::Result<()> {
    tracing::info!(
        ">>> Worker Trigger 1 [Broadcast] received: '{}' (id: {}), port: {}, db_url: {}",
        payload,
        id,
        port,
        db_url
    );
    Ok(())
}

#[trigger(template = "queue", target = TaskQueue)]
pub async fn worker_trigger2(payload: String, id: String, port: Arc<Port>) -> anyhow::Result<()> {
    tracing::info!(
        ">>> Worker Trigger 2 [Broadcast] received: '{}' (id: {}), port: {}",
        payload,
        id,
        port
    );
    Ok(())
}

// --- Load-Balancing Queue Trigger ---
// WorkerQueue is an LBQueue - messages are distributed to one handler at a time
#[trigger(template = "lb_queue", target = WorkerQueue)]
pub async fn lb_worker_trigger(payload: String, id: String, port: Arc<Port>) -> anyhow::Result<()> {
    tracing::info!(
        ">>> LB Worker Trigger [LoadBalancing] received: '{}' (id: {}), port: {}",
        payload,
        id,
        port
    );
    Ok(())
}

// --- Signal Trigger ---
// Uses the Notify provider for event signaling
#[trigger(template = "event", target = UserNotifier)]
pub async fn notify_trigger(_payload: (), id: String, port: Arc<Port>) -> anyhow::Result<()> {
    tracing::info!(
        ">>> Notify Trigger [Event] received (id: {}), port: {}",
        id,
        port
    );
    Ok(())
}
