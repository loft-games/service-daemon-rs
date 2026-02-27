//! Trigger handler definitions.
//!
//! Each handler subscribes to a provider defined in `providers.rs`.
//! Triggers are **decoupled** from services: they execute independently
//! and are registered in the daemon's global registry via the `#[trigger]` macro.

use crate::providers::{CleanupSchedule, ExternalStatus, TaskQueue, UserNotifier, WorkerQueue};
use service_daemon::TT::*;
use service_daemon::{publish, trigger};
use std::sync::Arc;

// =============================================================================
// Cron Trigger
// =============================================================================

/// Fires every 30 seconds (per `CleanupSchedule`).
#[trigger(Cron(CleanupSchedule))]
pub async fn cleanup_trigger() -> anyhow::Result<()> {
    tracing::info!(">>> [Cron] Cleanup trigger fired");
    Ok(())
}

// =============================================================================
// Broadcast Queue Triggers
// =============================================================================

/// Handler 1: receives ALL messages from `TaskQueue`.
#[trigger(Queue(TaskQueue))]
pub async fn broadcast_handler_a(payload: String) -> anyhow::Result<()> {
    tracing::info!(">>> [Broadcast A] received: '{}'", payload);
    Ok(())
}

/// Handler 2: also receives ALL messages from `TaskQueue`.
/// This demonstrates the fanout behavior of broadcast queues.
#[trigger(BQueue(TaskQueue))]
pub async fn broadcast_handler_b(payload: String) -> anyhow::Result<()> {
    tracing::info!(">>> [Broadcast B] received: '{}'", payload);
    Ok(())
}

// =============================================================================
// Load-Balancing Queue Trigger
// =============================================================================

/// Receives messages from `WorkerQueue` in a round-robin fashion.
/// Only ONE handler gets each message.
#[trigger(LBQueue(WorkerQueue))]
pub async fn lb_worker_handler(payload: String) -> anyhow::Result<()> {
    tracing::info!(">>> [LBQueue] received: '{}'", payload);
    Ok(())
}

// =============================================================================
// Complex Payload with Arc
// =============================================================================

/// Receives a `ComplexJob` wrapped in `Arc` -- zero-copy payload delivery.
#[trigger(LBQueue(crate::providers::JobQueue))]
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
#[trigger(Event(UserNotifier))]
pub async fn on_user_notified() -> anyhow::Result<()> {
    tracing::info!(">>> [Event] User notification received");
    Ok(())
}

// =============================================================================
// Watch Trigger
// =============================================================================

/// Fires whenever `ExternalStatus` is modified via its RwLock.
/// Receives a snapshot of the state AFTER the modification.
#[trigger(Watch(ExternalStatus))]
pub async fn on_external_status_changed(snapshot: Arc<ExternalStatus>) -> anyhow::Result<()> {
    tracing::info!(
        ">>> [Watch] ExternalStatus changed: '{}' (count: {})",
        snapshot.message,
        snapshot.updated_count
    );
    Ok(())
}

// =============================================================================
// Sync Trigger (via #[allow_sync] -- no async overhead)
// =============================================================================

/// Demonstrates that triggers can also be synchronous.
/// Uses `Notify` template (alias for `Event`).
#[service_daemon::allow_sync]
#[trigger(Notify(UserNotifier))]
pub fn sync_notify_trigger() -> anyhow::Result<()> {
    tracing::info!(">>> [Sync Event] Sync notify trigger fired");
    Ok(())
}

// =============================================================================
// Event Tracing Trigger -- two-hop chain demo
// =============================================================================

/// Captures the notification signal and publishes a processed
/// result to `TaskQueue`, creating a second ripple in the system.
///
/// Together with `on_user_notified` and `sync_notify_trigger`, this demonstrates
/// one signal triggering multiple handlers simultaneously.
#[trigger(Signal(UserNotifier))]
pub async fn on_tick() -> anyhow::Result<()> {
    tracing::info!("Tick signal captured! Processing...");

    // Simulate some work
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Publish the result to the shared broadcast queue
    publish("tick_processed", || async {
        let _ = TaskQueue::push("Tick processed successfully".to_string()).await;
    })
    .await;

    tracing::info!("Processing complete, result published to TaskQueue");
    Ok(())
}
