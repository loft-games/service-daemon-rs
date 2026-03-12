//! Public API functions for service lifecycle management.
//!
//! These functions are called from within service tasks to interact with
//! the daemon's state plane, shelf, and signaling mechanisms. They rely on
//! task-local storage (`CURRENT_SERVICE` / `CURRENT_RESOURCES`) set up by
//! the `#[service]` and `#[trigger]` macros.

use super::identity::{CURRENT_RESOURCES, CURRENT_SERVICE, DaemonResources, ServiceIdentity};
use std::any::{Any, TypeId};
use std::future::Future;
use std::sync::Arc;
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
    resources: Arc<DaemonResources>,
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
///
/// # Note
/// This function is `async` for API consistency with the rest of the context module
/// and to allow future migration to async-aware storage backends without breaking changes.
pub async fn shelve<T: Any + Send + Sync>(key: &str, data: T) {
    let name = match CURRENT_SERVICE.try_with(|id| id.name) {
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
///
/// # Note
/// This function is `async` for API consistency with the rest of the context module
/// and to allow future migration to async-aware storage backends without breaking changes.
pub async fn unshelve<T: Any + Send + Sync>(key: &str) -> Option<T> {
    let name = match CURRENT_SERVICE.try_with(|id| id.name) {
        Ok(n) => n,
        Err(_) => return None,
    };
    CURRENT_RESOURCES
        .try_with(|r| {
            r.shelf.get(name).and_then(|entry| {
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
///
/// # Note
/// This function is `async` for API consistency with the rest of the context module
/// and to allow future migration to async-aware storage backends without breaking changes.
pub async fn shelve_clone<T: Any + Clone + Send + Sync>(key: &str) -> Option<T> {
    let name = match CURRENT_SERVICE.try_with(|id| id.name) {
        Ok(n) => n,
        Err(_) => return None,
    };
    CURRENT_RESOURCES
        .try_with(|r| {
            r.shelf.get(name).and_then(|entry| {
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
// Trigger Configuration API
// ---------------------------------------------------------------------------

/// Retrieves a user-registered trigger configuration of type `T`.
///
/// Returns `Some(T)` if the user registered this config type via
/// [`ServiceDaemonBuilder::with_trigger_config`](crate::ServiceDaemonBuilder::with_trigger_config), otherwise `None`.
///
/// This function is typically called from the default `run_as_service`
/// implementation in [`TriggerHost`](crate::models::trigger::TriggerHost) to check for user overrides before
/// falling back to the template's self-declared [`ScalingPolicy`](crate::models::policy::ScalingPolicy).
///
/// # Panics
///
/// Returns `None` if called outside a service scope (no task-local context).
pub fn trigger_config<T: Any + Clone + Send + Sync>() -> Option<T> {
    CURRENT_RESOURCES
        .try_with(|resources| {
            resources
                .trigger_configs
                .get(&TypeId::of::<T>())
                .and_then(|entry| entry.value().downcast_ref::<T>().cloned())
        })
        .ok()
        .flatten()
}
