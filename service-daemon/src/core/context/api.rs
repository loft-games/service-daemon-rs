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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

use crate::core::diagnostics::{
    DiagnosticsStore, GenerationDiagnosticsHandle, SleepExitReason, SleepObservation,
    SleepObservationSource,
};
use crate::core::provider_scope::ProviderScope;
use crate::core::runtime_facts::TriggerRuntimeFactsHandle;
use crate::core::trigger_policy_overlay::{
    EffectiveTriggerPolicy, TriggerBasePolicy, TriggerPolicyOverlayStore,
};
use crate::models::{
    ServiceId, ServiceStatus, TriggerPolicyOverlay, TriggerPolicyOverlayError,
    TriggerPressureSnapshot,
};

/// Runs a future with service task-local identity and resources set.
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

pub(crate) async fn __run_daemon_resources_scope<F, Fut>(
    resources: Arc<DaemonResources>,
    f: F,
) -> Fut::Output
where
    F: FnOnce() -> Fut,
    Fut: Future,
{
    CURRENT_RESOURCES.scope(resources, f()).await
}

pub(crate) async fn __run_daemon_resources_sync_scope<F, T>(
    resources: Arc<DaemonResources>,
    f: F,
) -> T
where
    F: FnOnce() -> T,
{
    CURRENT_RESOURCES.scope(resources, async move { f() }).await
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
        resources
            .runtime_facts
            .record_service_status(id.service_id, &next_status);
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
/// The async signature matches the other context helpers.
pub async fn shelve<T: Any + Send + Sync>(key: &str, data: T) {
    let service_id = match CURRENT_SERVICE.try_with(|id| id.service_id) {
        Ok(id) => id,
        Err(_) => return,
    };
    if let Ok(resources) = CURRENT_RESOURCES.try_with(|r| r.clone()) {
        let entry = resources.shelf.entry(service_id).or_default();
        entry.insert(key.to_string(), Box::new(data));
    }
}

/// Retrieves a shelved managed value previously submitted by this service.
/// The value is **removed** from the service's isolated bucket.
///
/// For a non-destructive read, use [`shelve_clone`] instead.
///
/// # Note
/// The async signature matches the other context helpers.
pub async fn unshelve<T: Any + Send + Sync>(key: &str) -> Option<T> {
    let service_id = match CURRENT_SERVICE.try_with(|id| id.service_id) {
        Ok(id) => id,
        Err(_) => return None,
    };
    CURRENT_RESOURCES
        .try_with(|r| {
            r.shelf.get(&service_id).and_then(|entry| {
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
/// The async signature matches the other context helpers.
pub async fn shelve_clone<T: Any + Clone + Send + Sync>(key: &str) -> Option<T> {
    let service_id = match CURRENT_SERVICE.try_with(|id| id.service_id) {
        Ok(id) => id,
        Err(_) => return None,
    };
    CURRENT_RESOURCES
        .try_with(|r| {
            r.shelf.get(&service_id).and_then(|entry| {
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
        resources
            .runtime_facts
            .record_service_status(id.service_id, &ServiceStatus::Healthy);
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
///
/// **Spawn-safe**: captures lifecycle tokens at call-site so the returned
/// future works even when polled from a `tokio::spawn`'d task (e.g.
/// `axum::serve().with_graceful_shutdown(wait_shutdown())`) where
/// `task_local!` context is unavailable.
///
/// Outside a `#[service]` scope, falls back to process-level shutdown only
/// (skips `reload_token` -- an externally spawned task has no concept of
/// individual service reloads).
pub fn wait_shutdown() -> impl Future<Output = ()> + Send + 'static {
    implicit_handshake();

    // Must capture tokens here (not inside the async block) because
    // task_local is only accessible in the caller's task context.
    let tokens =
        CURRENT_SERVICE.try_with(|id| (id.cancellation_token.clone(), id.reload_token.clone()));
    let process_cancel = super::identity::process_token().clone();

    async move {
        match tokens {
            Ok((cancel, reload)) => {
                tokio::select! {
                    _ = cancel.cancelled() => {}
                    _ = reload.cancelled() => {}
                }
            }
            Err(_) => process_cancel.cancelled().await,
        }
    }
}

/// Returns a cancellation token suitable for provider resolution in the current context.
///
/// Inside a `#[service]` / `#[trigger]` scope, this derives a child token from the
/// current service cancellation token. Outside a managed service scope, it returns
/// a fresh standalone token so framework-external `resolve()` calls are not bound
/// to daemon/process lifecycle management.
#[doc(hidden)]
pub fn current_cancellation_token() -> tokio_util::sync::CancellationToken {
    CURRENT_SERVICE
        .try_with(|id| id.cancellation_token.child_token())
        .unwrap_or_else(|_| tokio_util::sync::CancellationToken::new())
}

pub(crate) fn current_provider_scope() -> Arc<ProviderScope> {
    CURRENT_RESOURCES
        .try_with(|resources| resources.provider_scope.clone())
        .unwrap_or_else(|_| ProviderScope::root())
}

/// An interruptible sleep that returns early if a shutdown or reload signal is received.
/// Returns `true` if the sleep completed normally, `false` if interrupted.
///
/// **Note**: If the service is still in a "Starting" phase, this function will
/// implicitly transition it to `Healthy` status.
pub async fn sleep(duration: Duration) -> bool {
    implicit_handshake();
    if let Ok(id) = CURRENT_SERVICE.try_with(|id| id.clone()) {
        let start = Instant::now();
        tokio::select! {
            _ = tokio::time::sleep(duration) => {
                let elapsed = start.elapsed();
                record_service_sleep_observation(
                    &id,
                    SleepExitReason::Completed,
                    duration,
                    elapsed,
                    elapsed.saturating_sub(duration),
                );
                true
            }
            _ = id.cancellation_token.cancelled() => {
                record_service_sleep_observation(
                    &id,
                    SleepExitReason::Shutdown,
                    duration,
                    start.elapsed(),
                    Duration::ZERO,
                );
                false
            }
            _ = id.reload_token.cancelled() => {
                record_service_sleep_observation(
                    &id,
                    SleepExitReason::Reload,
                    duration,
                    start.elapsed(),
                    Duration::ZERO,
                );
                false
            }
        }
    } else {
        // Outside of a service context, just perform a regular sleep
        tokio::time::sleep(duration).await;
        true
    }
}

fn record_service_sleep_observation(
    id: &ServiceIdentity,
    reason: SleepExitReason,
    requested: Duration,
    elapsed: Duration,
    drift: Duration,
) {
    if let Some(diagnostics) = id.diagnostics.as_ref() {
        diagnostics.record_sleep_observation(SleepObservation {
            source: SleepObservationSource::ServiceSleep,
            reason,
            requested,
            elapsed,
            drift,
        });
    }
}

pub(crate) fn current_generation_diagnostics() -> Option<GenerationDiagnosticsHandle> {
    CURRENT_SERVICE
        .try_with(|id| id.diagnostics.clone())
        .ok()
        .flatten()
}

pub(crate) fn current_daemon_diagnostics() -> Option<Arc<DiagnosticsStore>> {
    CURRENT_RESOURCES
        .try_with(|resources| resources.diagnostics.clone())
        .ok()
}

pub(crate) fn current_service_generation() -> u64 {
    CURRENT_SERVICE
        .try_with(|identity| {
            identity
                .diagnostics
                .as_ref()
                .map(|diagnostics| diagnostics.snapshot().generation)
        })
        .ok()
        .flatten()
        .unwrap_or_default()
}

pub(crate) fn current_trigger_pressure(service_id: ServiceId) -> Option<TriggerPressureSnapshot> {
    CURRENT_RESOURCES
        .try_with(|resources| resources.runtime_facts.trigger_pressure(service_id))
        .ok()
        .flatten()
}

pub(crate) fn request_trigger_policy_overlay(
    service_id: ServiceId,
    generation: u64,
    overlay: TriggerPolicyOverlay,
) -> Result<(), TriggerPolicyOverlayError> {
    CURRENT_RESOURCES
        .try_with(|resources| {
            resources
                .trigger_policy_overlays
                .request_overlay(service_id, generation, overlay)
        })
        .map_err(|_| TriggerPolicyOverlayError::TriggerOverlayUnavailable)?
}

pub(crate) fn clear_trigger_policy_overlay(
    service_id: ServiceId,
    generation: u64,
    reason: &str,
) -> Result<(), TriggerPolicyOverlayError> {
    CURRENT_RESOURCES
        .try_with(|resources| {
            resources
                .trigger_policy_overlays
                .clear_overlay(service_id, generation, reason)
        })
        .map_err(|_| TriggerPolicyOverlayError::TriggerOverlayUnavailable)?
}

pub(crate) fn register_current_trigger_runtime(
    service_id: ServiceId,
    service_name: &'static str,
    generation: u64,
    semaphore: Arc<Semaphore>,
    current_limit: Arc<AtomicUsize>,
) -> Option<TriggerRuntimeFactsHandle> {
    CURRENT_RESOURCES
        .try_with(|resources| {
            resources.runtime_facts.register_trigger(
                service_id,
                service_name,
                generation,
                semaphore,
                current_limit,
            )
        })
        .ok()
}

pub(crate) fn register_current_trigger_policy_overlay(
    service_id: ServiceId,
    generation: u64,
    base: TriggerBasePolicy,
    semaphore: Arc<Semaphore>,
    current_limit: Arc<AtomicUsize>,
) -> Option<Arc<TriggerPolicyOverlayStore>> {
    CURRENT_RESOURCES
        .try_with(|resources| {
            resources.trigger_policy_overlays.register_trigger(
                service_id,
                generation,
                base,
                semaphore,
                current_limit,
            );
            resources.trigger_policy_overlays.clone()
        })
        .ok()
}

pub(crate) fn effective_trigger_policy(
    store: &TriggerPolicyOverlayStore,
    service_id: ServiceId,
    generation: u64,
    fallback: TriggerBasePolicy,
) -> EffectiveTriggerPolicy {
    store.effective_policy(service_id, generation, fallback)
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
/// falling back to the template's self-declared [`ScalingPolicy`](crate::models::ScalingPolicy).
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

/// Spawns a new asynchronous task that inherits the current service identity and resources.
///
/// This is a convenience wrapper around `tokio::spawn` that automatically
/// captures `CURRENT_SERVICE` and `CURRENT_RESOURCES` from the caller's task
/// and applies them to the new task.
///
/// # Example
/// ```rust,ignore
/// use service_daemon::spawn_with_context;
///
/// spawn_with_context(async move {
///     // This task has access to the same shelve/state as the parent service
///     service_daemon::shelve("spawned", true).await;
/// });
/// ```
pub fn spawn_with_context<Fut>(fut: Fut) -> tokio::task::JoinHandle<Fut::Output>
where
    Fut: Future + Send + 'static,
    Fut::Output: Send + 'static,
{
    let context = CURRENT_SERVICE
        .try_with(|id| id.clone())
        .and_then(|identity| CURRENT_RESOURCES.try_with(|r| (identity, r.clone())));

    match context {
        Ok((identity, resources)) => {
            tokio::spawn(async move { __run_service_scope(identity, resources, || fut).await })
        }
        Err(_) => tokio::spawn(fut),
    }
}

/// Returns the `ServiceId` of the calling service.
///
/// Falls back to `ServiceId(0)` if called outside of a managed service scope
/// (e.g., in a background task spawned via `tokio::spawn` without context propagation).
pub fn current_service_id() -> ServiceId {
    CURRENT_SERVICE
        .try_with(|identity| identity.service_id)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diagnostics::{DiagnosticsStore, RuntimeLane};
    use crate::core::provider_scope::ProviderScopeId;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn current_provider_scope_falls_back_to_root_outside_service_scope() {
        let scope = current_provider_scope();

        assert_eq!(scope.id(), ProviderScopeId::root());
    }

    #[tokio::test]
    async fn daemon_resources_scope_sets_provider_scope_without_service_identity() {
        let resources = DaemonResources::new();
        let expected_scope_id = resources.provider_scope.id();

        let actual_scope_id =
            __run_daemon_resources_scope(resources, || async { current_provider_scope().id() })
                .await;

        assert_eq!(actual_scope_id, expected_scope_id);
        assert_ne!(actual_scope_id, ProviderScopeId::root());
    }

    #[tokio::test]
    async fn current_provider_scope_uses_daemon_resources_inside_service_scope() {
        let resources = DaemonResources::new();
        let expected_scope_id = resources.provider_scope.id();
        let identity = ServiceIdentity::new(
            ServiceId::new(17),
            "provider_scope",
            CancellationToken::new(),
            CancellationToken::new(),
        );

        let actual_scope_id = __run_service_scope(identity, resources, || async {
            current_provider_scope().id()
        })
        .await;

        assert_eq!(actual_scope_id, expected_scope_id);
        assert_ne!(actual_scope_id, ProviderScopeId::root());
    }

    #[tokio::test]
    async fn spawn_with_context_falls_back_outside_service_scope() {
        let task = spawn_with_context(async { 42u32 });

        let value = task.await.expect("fallback task should join cleanly");

        assert_eq!(value, 42);
    }

    #[tokio::test]
    async fn sleep_records_completed_diagnostics() {
        let store = DiagnosticsStore::new();
        let service_id = ServiceId::new(11);
        let diagnostics =
            store.register_generation(service_id, "sleep_completed", 1, RuntimeLane::Standard);
        let identity = ServiceIdentity::new_with_diagnostics(
            service_id,
            "sleep_completed",
            CancellationToken::new(),
            CancellationToken::new(),
            diagnostics,
        );

        let completed = __run_service_scope(identity, DaemonResources::new(), || async {
            sleep(Duration::ZERO).await
        })
        .await;

        assert!(completed);
        let generation = store
            .generation_snapshot(service_id, 1)
            .expect("generation diagnostics should exist");
        assert_eq!(generation.aggregate.service_sleep.completed, 1);
        assert_eq!(generation.aggregate.service_sleep.interrupted, 0);

        let lane = store.lane_snapshot(RuntimeLane::Standard);
        assert_eq!(lane.aggregate.service_sleep.completed, 1);
    }

    #[tokio::test]
    async fn sleep_records_reload_interruption_diagnostics() {
        let store = DiagnosticsStore::new();
        let service_id = ServiceId::new(12);
        let diagnostics =
            store.register_generation(service_id, "sleep_reload", 1, RuntimeLane::Standard);
        let reload_token = CancellationToken::new();
        reload_token.cancel();
        let identity = ServiceIdentity::new_with_diagnostics(
            service_id,
            "sleep_reload",
            CancellationToken::new(),
            reload_token,
            diagnostics,
        );

        let completed = __run_service_scope(identity, DaemonResources::new(), || async {
            sleep(Duration::from_secs(30)).await
        })
        .await;

        assert!(!completed);
        let generation = store
            .generation_snapshot(service_id, 1)
            .expect("generation diagnostics should exist");
        assert_eq!(generation.aggregate.service_sleep.completed, 0);
        assert_eq!(generation.aggregate.service_sleep.interrupted, 1);
        assert_eq!(generation.aggregate.service_sleep.total_drift_ms, 0);
    }
}
