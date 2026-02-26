//! Public API functions for service lifecycle management.
//!
//! These functions are called from within service tasks to interact with
//! the daemon's state plane, shelf, and signaling mechanisms. They rely on
//! task-local storage (`CURRENT_SERVICE` / `CURRENT_RESOURCES`) set up by
//! the `#[service]` and `#[trigger]` macros.

use super::identity::{CURRENT_RESOURCES, CURRENT_SERVICE, DaemonResources, ServiceIdentity};

use std::any::Any;
use std::future::Future;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::models::ServiceStatus;

/// Runs a future within the context of a service.
///
/// This is the internal entry point used by the `#[service]` and `#[trigger]` macros.
/// It sets up the task-local identity and resources before executing the user's code.
#[doc(hidden)]
pub async fn __run_service_scope<F, Fut>(
    identity: ServiceIdentity,
    resources: DaemonResources,
    f: F,
) -> Fut::Output
where
    F: FnOnce() -> Fut,
    Fut: Future,
{
    CURRENT_SERVICE
        .scope(identity, CURRENT_RESOURCES.scope(resources, f()))
        .await
}

/// Returns the current lifecycle status of the calling service.
pub fn state() -> ServiceStatus {
    let id = match CURRENT_SERVICE.try_with(|id| id.clone()) {
        Ok(id) => id,
        Err(_) => return ServiceStatus::Initializing,
    };

    // Fast path: Check cancellation tokens first (atomic, no locking)
    if id.cancellation_token.is_cancelled() {
        return ServiceStatus::ShuttingDown;
    }
    if id.reload_token.is_cancelled() {
        // Need to check if daemon already marked ShuttingDown
        if let Ok(resources) = CURRENT_RESOURCES.try_with(|r| r.clone())
            && let Some(status) = resources.status_plane.get(&id.service_id)
            && matches!(status.value(), ServiceStatus::ShuttingDown)
        {
            return ServiceStatus::ShuttingDown;
        }
        return ServiceStatus::NeedReload;
    }

    // Full status lookup
    CURRENT_RESOURCES
        .try_with(|r| {
            r.status_plane
                .get(&id.service_id)
                .map(|s| s.value().clone())
                .unwrap_or(ServiceStatus::Initializing)
        })
        .unwrap_or(ServiceStatus::Initializing)
}

/// Signals that the service has completed its current state (e.g. initialization or cleanup).
/// This will advance the service status to the next logical step based on the handshake protocol:
/// - `Initializing | Restoring | Recovering` -> `Healthy` (service is now ready).
/// - `NeedReload | ShuttingDown` -> `Terminated` (service is ready for collection).
/// - Otherwise, no-op.
pub fn done() {
    let id = match CURRENT_SERVICE.try_with(|id| id.clone()) {
        Ok(id) => id,
        Err(_) => return,
    };
    let resources = match CURRENT_RESOURCES.try_with(|r| r.clone()) {
        Ok(r) => r,
        Err(_) => return,
    };

    let current_status = resources
        .status_plane
        .get(&id.service_id)
        .map(|s| s.value().clone())
        .unwrap_or(ServiceStatus::Initializing);

    let next_status = match &current_status {
        ServiceStatus::Initializing | ServiceStatus::Restoring | ServiceStatus::Recovering(_) => {
            ServiceStatus::Healthy
        }
        ServiceStatus::NeedReload | ServiceStatus::ShuttingDown => ServiceStatus::Terminated,
        _ => current_status.clone(), // No-op for Healthy and Terminated
    };

    if next_status != current_status {
        resources
            .status_plane
            .insert(id.service_id, next_status.clone());
        resources.status_changed.notify_waiters();
        tracing::info!(
            "Service '{}' signalled done() (Transition: {:?} -> {:?})",
            id.name,
            current_status,
            next_status
        );
    }
}

/// Shelves a managed value to the daemon. This value will survive service reloads and crashes.
/// The value is stored in a service-isolated bucket based on the calling service's identity.
pub async fn shelve<T: Any + Send + Sync>(key: &str, data: T) {
    let name = match CURRENT_SERVICE.try_with(|id| id.name.clone()) {
        Ok(n) => n,
        Err(_) => return,
    };
    if let Ok(resources) = CURRENT_RESOURCES.try_with(|r| r.clone()) {
        let entry = resources.shelf.entry(name).or_default();
        entry.insert(key.to_string(), Box::new(data));
    }
}

/// Retrieves a shelved managed value previously submitted by this service.
/// The value is **removed** from the service's isolated bucket.
///
/// For a non-destructive read, use [`shelve_clone`] instead.
pub async fn unshelve<T: Any + Send + Sync>(key: &str) -> Option<T> {
    let name = match CURRENT_SERVICE.try_with(|id| id.name.clone()) {
        Ok(n) => n,
        Err(_) => return None,
    };
    CURRENT_RESOURCES
        .try_with(|r| {
            r.shelf.get(&name).and_then(|entry| {
                entry
                    .remove(key)
                    .and_then(|(_, val)| val.downcast::<T>().ok().map(|b| *b))
            })
        })
        .ok()
        .flatten()
}

/// Retrieves a **clone** of a shelved value without removing it from the shelf.
///
/// This is useful when trigger hosts need to access the same shelved state
/// across multiple `handle_step` iterations (e.g., a bridge `Arc<Notify>`
/// for cron triggers).
///
/// # Requirements
/// The stored type `T` must implement `Clone`. This is naturally satisfied
/// by `Arc<T>` values, which are the primary use case.
pub async fn shelve_clone<T: Any + Clone + Send + Sync>(key: &str) -> Option<T> {
    let name = match CURRENT_SERVICE.try_with(|id| id.name.clone()) {
        Ok(n) => n,
        Err(_) => return None,
    };
    CURRENT_RESOURCES
        .try_with(|r| {
            r.shelf.get(&name).and_then(|entry| {
                entry
                    .get(key)
                    .and_then(|val| val.downcast_ref::<T>().cloned())
            })
        })
        .ok()
        .flatten()
}

/// Performs an implicit handshake if the service is still in a "Starting" phase.
/// This is an optimized version that uses a local flag to avoid repeated global lookups.
fn implicit_handshake() {
    let id = match CURRENT_SERVICE.try_with(|id| id.clone()) {
        Ok(id) => id,
        Err(_) => return,
    };

    // Fast path: If already handshaked this generation, skip entirely
    if id.is_handshake_done.load(Ordering::Relaxed) {
        return;
    }

    let resources = match CURRENT_RESOURCES.try_with(|r| r.clone()) {
        Ok(r) => r,
        Err(_) => return,
    };

    // Check and transition startup states
    let needs_transition = resources
        .status_plane
        .get(&id.service_id)
        .map(|s| {
            matches!(
                s.value(),
                ServiceStatus::Initializing
                    | ServiceStatus::Restoring
                    | ServiceStatus::Recovering(_)
            )
        })
        .unwrap_or(false);

    if needs_transition {
        resources
            .status_plane
            .insert(id.service_id, ServiceStatus::Healthy);
        resources.status_changed.notify_waiters();
        tracing::debug!(
            "Service '{}' implicitly transitioned to Healthy (via lifecycle utility)",
            id.name
        );
    }

    // Mark as done for this task; subsequent calls skip all of the above
    id.is_handshake_done.store(true, Ordering::Relaxed);
}

/// Checks if the current service or the daemon has been signaled to stop or reload.
/// Returns `true` for any "descending" status (NeedReload, ShuttingDown, Terminated).
///
/// **Note**: If the service is still in a "Starting" phase, this function will
/// implicitly transition it to `Healthy` status (once per task lifetime).
pub fn is_shutdown() -> bool {
    implicit_handshake();

    // Fast path: Check tokens directly (atomic, no locking)
    if let Ok(id) = CURRENT_SERVICE.try_with(|id| id.clone())
        && (id.cancellation_token.is_cancelled() || id.reload_token.is_cancelled())
    {
        return true;
    }
    false
}

/// Waits until the service is notified to stop or reload.
/// This future completes when the service's cancellation or reload token is triggered.
///
/// **Note**: If the service is still in a "Starting" phase, this function will
/// implicitly transition it to `Healthy` status.
pub async fn wait_shutdown() {
    implicit_handshake();
    if let Ok(id) = CURRENT_SERVICE.try_with(|id| id.clone()) {
        tokio::select! {
            _ = id.cancellation_token.cancelled() => {}
            _ = id.reload_token.cancelled() => {}
        }
    }
}

/// An interruptible sleep that returns early if a shutdown or reload signal is received.
/// Returns `true` if the sleep completed normally, `false` if interrupted.
///
/// **Note**: If the service is still in a "Starting" phase, this function will
/// implicitly transition it to `Healthy` status.
pub async fn sleep(duration: Duration) -> bool {
    implicit_handshake();
    if let Ok(id) = CURRENT_SERVICE.try_with(|id| id.clone()) {
        tokio::select! {
            _ = tokio::time::sleep(duration) => true,
            _ = id.cancellation_token.cancelled() => false,
            _ = id.reload_token.cancelled() => false,
        }
    } else {
        // Outside of a service context, just perform a regular sleep
        tokio::time::sleep(duration).await;
        true
    }
}

// ---------------------------------------------------------------------------
// Event Publishing API — "Throwing stones into the water"
// ---------------------------------------------------------------------------

/// Generates a globally unique message ID for event tracing.
///
/// This is useful when you need to manually call a provider's `.push()` or
/// `.notify()` method and want to correlate the event with a message ID.
///
/// # Example
/// ```rust,ignore
/// let message_id = service_daemon::generate_message_id();
/// tracing::info!(%message_id, "Publishing event");
/// MyQueue::push(payload).await;
/// ```
pub fn generate_message_id() -> String {
    crate::models::trigger::generate_message_id()
}

/// Publishes an event from the current service context with full traceability.
///
/// This is the canonical way for a service to "throw a stone" — it wraps
/// a provider call (e.g. `push()` or `notify()`) with structured tracing
/// metadata including the source `ServiceId` and a unique `message_id`.
///
/// The closure receives no arguments; it should capture the provider and
/// payload from the enclosing scope.
///
/// # Example
/// ```rust,ignore
/// use service_daemon::{publish, service};
/// use std::sync::Arc;
///
/// #[service]
/// pub async fn my_service(queue: Arc<TaskQueue>) -> anyhow::Result<()> {
///     service_daemon::done();
///     while !service_daemon::is_shutdown() {
///         publish("order_created", async {
///             TaskQueue::push("new order".to_string()).await;
///         }).await;
///         service_daemon::sleep(std::time::Duration::from_secs(5)).await;
///     }
///     Ok(())
/// }
/// ```
pub async fn publish<F, Fut>(event_name: &str, action: F)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let message_id = generate_message_id();
    let source_id = CURRENT_SERVICE
        .try_with(|id| id.service_id)
        .unwrap_or(crate::models::ServiceId::new(0));

    let span = tracing::info_span!(
        "publish",
        event = %event_name,
        %message_id,
        source = %source_id,
    );

    let _guard = span.enter();
    tracing::info!("Event published");
    drop(_guard);

    // Execute the user's publish action within the span
    use tracing::Instrument;
    action().instrument(span).await;
}
