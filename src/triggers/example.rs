use std::sync::Arc;

use crate::providers::trigger_providers::{CleanupSchedule, TaskQueue, UserNotifier};
use crate::providers::typed_providers::{DbUrl, Port};
use service_daemon::trigger;

#[trigger(template = "cron", target = CleanupSchedule)]
pub async fn cleanup_trigger(_payload: (), id: String, port: Arc<Port>) -> anyhow::Result<()> {
    tracing::info!(
        ">>> Cleanup Trigger [Cron] fired (id: {}), port: {}",
        id,
        port
    );
    Ok(())
}

#[trigger(template = "queue", target = TaskQueue)]
pub async fn worker_trigger(
    payload: String,
    id: String,
    port: Arc<Port>,
    db_url: Arc<DbUrl>,
) -> anyhow::Result<()> {
    tracing::info!(
        ">>> Worker Trigger [Queue] received: '{}' (id: {}), port: {}, db_url: {}",
        payload,
        id,
        port,
        db_url
    );
    Ok(())
}

#[trigger(template = "custom", target = UserNotifier)]
pub async fn notify_trigger(_payload: (), id: String, port: Arc<Port>) -> anyhow::Result<()> {
    tracing::info!(
        ">>> Notify Trigger [Custom] received (id: {}), port: {}",
        id,
        port
    );
    Ok(())
}
