//! Trigger host implementations — built-in engines for the trigger system.
//!
//! Each trigger host implements the `TriggerHost` trait from `models::trigger`,
//! providing a pluggable event-loop that listens for events and dispatches them
//! to user-defined handlers wrapped in `TriggerContext`.
//!
//! The legacy public functions (`signal_trigger_host`, `queue_trigger_host`, etc.)
//! are preserved for backward compatibility with existing macro-generated code.
//! New trigger templates should implement `TriggerHost` directly.

use chrono::Utc;
use futures::future::BoxFuture;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{Mutex, OnceCell};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, error, info, warn};

use crate::core::context;
use crate::models::service::ServiceId;
use crate::models::trigger::{TriggerContext, TriggerMessage};

// ---------------------------------------------------------------------------
// Shared utilities
// ---------------------------------------------------------------------------

/// Generates a globally unique message ID for each trigger event.
pub(crate) fn generate_message_id() -> String {
    #[cfg(feature = "uuid-trigger-ids")]
    {
        uuid::Uuid::new_v4().to_string()
    }
    #[cfg(not(feature = "uuid-trigger-ids"))]
    {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("msg-{}", id)
    }
}

/// Per-host monotonic sequence counter for trigger instance IDs.
///
/// Each trigger service gets its own `AtomicU64` via a local variable,
/// producing instance IDs like `svc#2:0`, `svc#2:1`, etc.
fn next_instance_seq(counter: &AtomicU64) -> u64 {
    counter.fetch_add(1, Ordering::Relaxed)
}

/// Constructs a `TriggerContext` with proper traceability fields.
fn build_context<P>(
    service_id: ServiceId,
    instance_counter: &AtomicU64,
    payload: P,
) -> TriggerContext<P> {
    TriggerContext {
        service_id,
        instance_seq: next_instance_seq(instance_counter),
        message: TriggerMessage {
            message_id: generate_message_id(),
            source_id: service_id,
            timestamp: Utc::now(),
            payload,
        },
    }
}

/// Attempts to retrieve the current service's `ServiceId` from the task-local
/// context. Falls back to `ServiceId(0)` if called outside a service scope
/// (e.g. during unit tests).
fn current_service_id() -> ServiceId {
    // The task-local CURRENT_SERVICE is set by the runner when spawning
    // a service. If we are inside a running service, this will succeed.
    context::identity::CURRENT_SERVICE
        .try_with(|identity| identity.service_id)
        .unwrap_or(ServiceId::new(0))
}

// ---------------------------------------------------------------------------
// Cron shared scheduler
// ---------------------------------------------------------------------------

/// Global shared scheduler for all cron triggers.
/// Using tokio::sync::OnceCell for async-native initialization.
#[cfg(feature = "cron")]
static SHARED_SCHEDULER: OnceCell<tokio_cron_scheduler::JobScheduler> = OnceCell::const_new();

#[cfg(feature = "cron")]
async fn get_shared_scheduler() -> anyhow::Result<tokio_cron_scheduler::JobScheduler> {
    SHARED_SCHEDULER
        .get_or_try_init(|| async {
            let sched = tokio_cron_scheduler::JobScheduler::new().await?;
            sched.start().await?;
            Ok::<_, anyhow::Error>(sched)
        })
        .await
        .cloned()
}

// ===========================================================================
// Signal (Notify) Trigger Host
// ===========================================================================

/// Signal-based trigger host.
///
/// Listens on a `tokio::sync::Notify` and fires the handler each time the
/// notify is triggered. Ideal for lightweight, payload-free events.
pub async fn signal_trigger_host<F>(
    name: &str,
    notifier: Arc<tokio::sync::Notify>,
    handler: F,
    _cancellation_token: CancellationToken,
) -> anyhow::Result<()>
where
    F: Fn() -> BoxFuture<'static, anyhow::Result<()>> + Clone + Send + Sync + 'static,
{
    let service_id = current_service_id();
    let instance_counter = AtomicU64::new(0);

    while !context::is_shutdown() {
        tokio::select! {
            _ = notifier.notified() => {
                let context = build_context(service_id, &instance_counter, ());
                let instance_id = context.trigger_instance_id();
                let message_id = context.message.message_id.clone();
                let span = tracing::info_span!("trigger", %name, %instance_id, %message_id);
                let h = handler.clone();
                async move {
                    info!("Signal trigger fired");
                    if let Err(e) = h().await {
                        error!("Trigger error: {:?}", e);
                    }
                }.instrument(span).await;
            }
            _ = context::wait_shutdown() => {
                info!("Signal trigger '{}' received shutdown, exiting", name);
                break;
            }
        }
    }
    Ok(())
}

// ===========================================================================
// Broadcast Queue Trigger Host
// ===========================================================================

/// Broadcast queue trigger host (fan-out).
///
/// Subscribes to a `tokio::sync::broadcast` channel and delivers every
/// received message to the handler. All subscribers see all messages.
pub async fn queue_trigger_host<T, F>(
    name: &str,
    mut receiver: tokio::sync::broadcast::Receiver<T>,
    handler: F,
    _cancellation_token: CancellationToken,
) -> anyhow::Result<()>
where
    T: Clone + Send + Sync + 'static,
    F: Fn(T) -> BoxFuture<'static, anyhow::Result<()>> + Clone + Send + Sync + 'static,
{
    let service_id = current_service_id();
    let instance_counter = AtomicU64::new(0);

    while !context::is_shutdown() {
        tokio::select! {
            res = receiver.recv() => {
                match res {
                    Ok(value) => {
                        let context = build_context(service_id, &instance_counter, ());
                        let instance_id = context.trigger_instance_id();
                        let message_id = context.message.message_id.clone();
                        let span = tracing::info_span!("trigger", %name, %instance_id, %message_id);
                        let h = handler.clone();
                        async move {
                            info!("Queue trigger received item");
                            if let Err(e) = h(value).await {
                                error!("Trigger error: {:?}", e);
                            }
                        }.instrument(span).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Queue trigger '{}' lagged by {} messages", name, n);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        warn!("Queue trigger '{}' channel closed", name);
                        break;
                    }
                }
            }
            _ = context::wait_shutdown() => {
                info!("Queue trigger '{}' received shutdown, exiting", name);
                break;
            }
        }
    }
    Ok(())
}

// ===========================================================================
// Load-Balancing Queue Trigger Host
// ===========================================================================

/// Load-balancing queue trigger host.
///
/// Consumes messages from a shared `tokio::sync::mpsc` channel behind a
/// `Mutex`. Only one subscriber processes each message (competing consumers).
pub async fn lb_queue_trigger_host<T, F>(
    name: &str,
    receiver_mutex: Arc<Mutex<tokio::sync::mpsc::Receiver<T>>>,
    handler: F,
    _cancellation_token: CancellationToken,
) -> anyhow::Result<()>
where
    T: Send + Sync + 'static,
    F: Fn(T) -> BoxFuture<'static, anyhow::Result<()>> + Clone + Send + Sync + 'static,
{
    let service_id = current_service_id();
    let instance_counter = AtomicU64::new(0);

    while !context::is_shutdown() {
        let item = tokio::select! {
            result = async {
                let mut receiver = receiver_mutex.lock().await;
                receiver.recv().await
            } => result,
            _ = context::wait_shutdown() => {
                info!("LB Queue trigger '{}' received shutdown, exiting", name);
                return Ok(());
            }
        };

        match item {
            Some(value) => {
                let context = build_context(service_id, &instance_counter, ());
                let instance_id = context.trigger_instance_id();
                let message_id = context.message.message_id.clone();
                let span = tracing::info_span!("trigger", %name, %instance_id, %message_id);
                let h = handler.clone();
                async move {
                    info!("LB Queue trigger received item");
                    if let Err(e) = h(value).await {
                        error!("Trigger error: {:?}", e);
                    }
                }
                .instrument(span)
                .await;
            }
            None => {
                warn!("LB Queue trigger '{}' channel closed", name);
                break;
            }
        }
    }
    Ok(())
}

// ===========================================================================
// Cron Trigger Host
// ===========================================================================

/// Cron-based scheduled trigger host.
///
/// Registers a job with the shared `tokio-cron-scheduler` and fires the
/// handler on each cron tick. The job is automatically removed on shutdown.
#[cfg(feature = "cron")]
pub async fn cron_trigger_host<F>(
    name: &str,
    schedule: &str,
    handler: F,
    _cancellation_token: CancellationToken,
) -> anyhow::Result<()>
where
    F: Fn() -> BoxFuture<'static, anyhow::Result<()>> + Clone + Send + Sync + 'static,
{
    use tokio_cron_scheduler::Job;

    let sched = get_shared_scheduler().await?;
    let handler = Arc::new(handler);
    let name_str = name.to_string();
    // Cron jobs run outside the task-local scope, so we capture the service ID
    // at registration time for use inside the job closure.
    let service_id = current_service_id();
    let instance_counter = Arc::new(AtomicU64::new(0));

    let job = Job::new_async(schedule, move |_uuid, _lock| {
        let h = handler.clone();
        let n = name_str.clone();
        let counter = instance_counter.clone();
        Box::pin(async move {
            let context = build_context(service_id, &counter, ());
            let instance_id = context.trigger_instance_id();
            let message_id = context.message.message_id.clone();
            let span = tracing::info_span!("trigger", name = %n, %instance_id, %message_id);
            async move {
                info!("Cron trigger fired");
                if let Err(e) = h().await {
                    error!("Trigger error: {:?}", e);
                }
            }
            .instrument(span)
            .await;
        })
    })?;

    let job_id = sched.add(job).await?;

    // For cron, we wait for shutdown.
    // The shared scheduler manages the execution in the background.
    context::wait_shutdown().await;

    // Remove the job from the shared scheduler before exiting
    if let Err(e) = sched.remove(&job_id).await {
        error!(
            "Failed to remove cron job '{}' from shared scheduler: {:?}",
            name, e
        );
    } else {
        info!("Removed cron job '{}' from shared scheduler", name);
    }

    Ok(())
}

// ===========================================================================
// Watch (State Change) Trigger Host
// ===========================================================================

/// State-watch trigger host.
///
/// Resolves a snapshot of the target provider and passes it to the handler.
/// The service is restarted (via the dependency watcher) whenever the target
/// state changes, producing a fresh invocation with an updated snapshot.
pub async fn watch_trigger_host<T, F>(
    name: &str,
    handler: F,
    _cancellation_token: CancellationToken,
) -> anyhow::Result<()>
where
    T: crate::core::di::Provided + Send + Sync + 'static,
    F: Fn(Arc<T>) -> BoxFuture<'static, anyhow::Result<()>> + Clone + Send + Sync + 'static,
{
    let service_id = current_service_id();
    let instance_counter = AtomicU64::new(0);

    // Resolve a fresh snapshot to pass to the handler
    let snapshot = T::resolve().await;
    let context = build_context(service_id, &instance_counter, ());
    let instance_id = context.trigger_instance_id();
    let message_id = context.message.message_id.clone();
    let span = tracing::info_span!("trigger", %name, %instance_id, %message_id);
    let h = handler.clone();

    async move {
        info!("Watch trigger fired (instance started)");
        if let Err(e) = h(snapshot).await {
            error!("Trigger error: {:?}", e);
        }
    }
    .instrument(span)
    .await;

    // Wait for the next reload (triggered by dependency watcher) or shutdown
    context::wait_shutdown().await;
    Ok(())
}
