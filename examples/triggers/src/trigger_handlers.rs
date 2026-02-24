//! Trigger handler definitions.
//!
//! Each handler subscribes to a provider defined in `providers.rs`.
//! Triggers are **decoupled** from services: they execute independently
//! and are registered in the daemon's global registry via the `#[trigger]` macro.

use crate::providers::{CleanupSchedule, ExternalStatus, TaskQueue, UserNotifier, WorkerQueue};
use service_daemon::trigger;
use std::sync::Arc;

// =============================================================================
// Cron Trigger
// =============================================================================

/// Fires every 30 seconds (per `CleanupSchedule`).
#[trigger(template = Cron, target = CleanupSchedule)]
pub async fn cleanup_trigger() -> anyhow::Result<()> {
    tracing::info!(">>> [Cron] Cleanup trigger fired");
    Ok(())
}

// =============================================================================
// Broadcast Queue Triggers
// =============================================================================

/// Handler 1: receives ALL messages from `TaskQueue`.
#[trigger(template = Queue, target = TaskQueue)]
pub async fn broadcast_handler_a(payload: String) -> anyhow::Result<()> {
    tracing::info!(">>> [Broadcast A] received: '{}'", payload);
    Ok(())
}

/// Handler 2: also receives ALL messages from `TaskQueue`.
/// This demonstrates the fanout behavior of broadcast queues.
#[trigger(template = BQueue, target = TaskQueue)]
pub async fn broadcast_handler_b(payload: String) -> anyhow::Result<()> {
    tracing::info!(">>> [Broadcast B] received: '{}'", payload);
    Ok(())
}

// =============================================================================
// Load-Balancing Queue Trigger
// =============================================================================

/// Receives messages from `WorkerQueue` in a round-robin fashion.
/// Only ONE handler gets each message.
#[trigger(template = LBQueue, target = WorkerQueue)]
pub async fn lb_worker_handler(payload: String) -> anyhow::Result<()> {
    tracing::info!(">>> [LBQueue] received: '{}'", payload);
    Ok(())
}

// =============================================================================
// Complex Payload with Arc
// =============================================================================

/// Receives a `ComplexJob` wrapped in `Arc` — zero-copy payload delivery.
#[trigger(template = LBQueue, target = crate::providers::JobQueue)]
pub async fn complex_job_handler(
    #[payload] job: Arc<crate::providers::ComplexJob>,
) -> anyhow::Result<()> {
    tracing::info!(">>> [LBQueue Complex] id={}, data='{}'", job.id, job.data);
    Ok(())
}

// =============================================================================
// Signal (Event) Trigger
// =============================================================================

/// Fires whenever `UserNotifier::notify()` is called.
#[trigger(template = Event, target = UserNotifier)]
pub async fn on_user_notified() -> anyhow::Result<()> {
    tracing::info!(">>> [Event] User notification received");
    Ok(())
}

// =============================================================================
// Watch Trigger
// =============================================================================

/// Fires whenever `ExternalStatus` is modified via its RwLock.
/// Receives a snapshot of the state AFTER the modification.
#[trigger(template = TT::Watch, target = ExternalStatus)]
pub async fn on_external_status_changed(snapshot: Arc<ExternalStatus>) -> anyhow::Result<()> {
    tracing::info!(
        ">>> [Watch] ExternalStatus changed: '{}' (count: {})",
        snapshot.message,
        snapshot.updated_count
    );
    Ok(())
}

// =============================================================================
// Sync Trigger (via #[allow_sync] — no async overhead)
// =============================================================================

/// Demonstrates that triggers can also be synchronous.
/// Uses `Notify` template (alias for `Event`).
#[service_daemon::allow_sync]
#[trigger(template = Notify, target = UserNotifier)]
pub fn sync_notify_trigger() -> anyhow::Result<()> {
    tracing::info!(">>> [Sync Event] Sync notify trigger fired");
    Ok(())
}
