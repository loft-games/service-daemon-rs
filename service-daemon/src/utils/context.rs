use dashmap::DashMap;
use std::any::Any;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::task_local;
use tokio_util::sync::CancellationToken;

use crate::models::ServiceStatus;

// Type aliases for the Shelf
type ShelfValue = Box<dyn Any + Send + Sync>;
type ServiceShelf = DashMap<String, ShelfValue>;
type GlobalShelfMapping = DashMap<String, ServiceShelf>;

/// Shared daemon resources that are owned by `ServiceDaemon` and plumbed to services.
///
/// This struct holds the references to daemon-managed resources. It is passed
/// to each service via the `CURRENT_SERVICE` task-local, enabling services to
/// interact with the daemon's state plane, shelf, and signaling mechanisms
/// without polluting the global namespace.
#[derive(Clone)]
pub struct DaemonResources {
    /// The unified Status Plane: stores the current lifecycle status for each service.
    pub status_plane: Arc<DashMap<String, ServiceStatus>>,
    /// Shelf for cross-generational state persistence (managed values).
    /// Structure: DashMap<ServiceName, DashMap<Key, Value>>
    pub shelf: Arc<GlobalShelfMapping>,
    /// Signals for services to reload.
    pub reload_signals: Arc<DashMap<String, Arc<tokio::sync::Notify>>>,
    /// Global notification for any status change in the STATUS_PLANE.
    pub status_changed: Arc<tokio::sync::Notify>,
}

impl DaemonResources {
    /// Creates a new set of daemon resources.
    pub fn new() -> Self {
        Self {
            status_plane: Arc::new(DashMap::new()),
            shelf: Arc::new(DashMap::new()),
            reload_signals: Arc::new(DashMap::new()),
            status_changed: Arc::new(tokio::sync::Notify::new()),
        }
    }
}

impl Default for DaemonResources {
    fn default() -> Self {
        Self::new()
    }
}

/// Internal identity of a service used to link task-local calls to the daemon's management.
///
/// This is a lightweight handle containing only the lifecycle tokens and a local
/// "handshake done" flag. The actual daemon resources are stored separately in
/// `CURRENT_RESOURCES` (internal, not exposed to users).
#[derive(Clone)]
pub struct ServiceIdentity {
    pub name: String,
    pub cancellation_token: CancellationToken,
    pub reload_token: CancellationToken,
    /// Shared flag: true means the auto-handshake (Initializing->Healthy) has been performed.
    /// Uses Arc to persist the state across TLS clones within the same task generation.
    handshake_done: Arc<AtomicBool>,
}

impl ServiceIdentity {
    /// Creates a new ServiceIdentity with the handshake flag set to false.
    pub fn new(
        name: String,
        cancellation_token: CancellationToken,
        reload_token: CancellationToken,
    ) -> Self {
        Self {
            name,
            cancellation_token,
            reload_token,
            handshake_done: Arc::new(AtomicBool::new(false)),
        }
    }
}

task_local! {
    /// Internal: The identity of the currently running service task.
    pub(crate) static CURRENT_SERVICE: ServiceIdentity;
    /// Internal: The daemon resources for the current service task.
    pub(crate) static CURRENT_RESOURCES: DaemonResources;
}

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
            && let Some(status) = resources.status_plane.get(&id.name)
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
                .get(&id.name)
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
        .get(&id.name)
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
            .insert(id.name.clone(), next_status.clone());
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
/// The value is retrieved from the service's isolated bucket.
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

/// Performs an implicit handshake if the service is still in a "Starting" phase.
/// This is an optimized version that uses a local flag to avoid repeated global lookups.
fn implicit_handshake() {
    let id = match CURRENT_SERVICE.try_with(|id| id.clone()) {
        Ok(id) => id,
        Err(_) => return,
    };

    // Fast path: If already handshaked this generation, skip entirely
    if id.handshake_done.load(Ordering::Relaxed) {
        return;
    }

    let resources = match CURRENT_RESOURCES.try_with(|r| r.clone()) {
        Ok(r) => r,
        Err(_) => return,
    };

    // Check and transition startup states
    let needs_transition = resources
        .status_plane
        .get(&id.name)
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
            .insert(id.name.clone(), ServiceStatus::Healthy);
        resources.status_changed.notify_waiters();
        tracing::debug!(
            "Service '{}' implicitly transitioned to Healthy (via lifecycle utility)",
            id.name
        );
    }

    // Mark as done for this task; subsequent calls skip all of the above
    id.handshake_done.store(true, Ordering::Relaxed);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_resources() -> DaemonResources {
        DaemonResources::new()
    }

    fn create_test_identity(name: &str) -> ServiceIdentity {
        ServiceIdentity::new(
            name.to_string(),
            CancellationToken::new(),
            CancellationToken::new(),
        )
    }

    /// Helper to run a future in a service scope (for tests).
    async fn in_scope<F, Fut, T>(identity: ServiceIdentity, resources: DaemonResources, f: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        CURRENT_SERVICE
            .scope(identity, CURRENT_RESOURCES.scope(resources, f()))
            .await
    }

    #[tokio::test]
    async fn test_shelve_unshelve() {
        let resources = create_test_resources();
        let identity = create_test_identity("test_service");

        in_scope(identity, resources, || async {
            shelve("test", 42i32).await;
            let val: Option<i32> = unshelve("test").await;
            assert_eq!(val, Some(42));

            // Verify it's removed after unshelve
            let val2: Option<i32> = unshelve("test").await;
            assert_eq!(val2, None);
        })
        .await;
    }

    #[tokio::test]
    async fn test_state_transitions() {
        let resources = create_test_resources();
        let identity = create_test_identity("state_service");

        resources
            .status_plane
            .insert("state_service".to_string(), ServiceStatus::NeedReload);

        in_scope(identity, resources, || async {
            assert!(matches!(state(), ServiceStatus::NeedReload));
        })
        .await;
    }

    #[tokio::test]
    async fn test_handshake_protocol() {
        let resources = create_test_resources();

        // Start in Initializing
        resources
            .status_plane
            .insert("handshake_service".to_string(), ServiceStatus::Initializing);

        let identity = create_test_identity("handshake_service");
        let resources_clone = resources.clone();
        in_scope(identity, resources.clone(), || async move {
            // After done(), status should become Healthy
            done();
            let status = resources_clone
                .status_plane
                .get("handshake_service")
                .map(|s| s.clone());
            assert_eq!(status, Some(ServiceStatus::Healthy));
        })
        .await;

        // Now test the descending phase
        resources
            .status_plane
            .insert("handshake_service".to_string(), ServiceStatus::NeedReload);

        let identity2 = create_test_identity("handshake_service");
        let resources_clone2 = resources.clone();
        in_scope(identity2, resources.clone(), || async move {
            // After done(), status should become Terminated
            done();
            let status = resources_clone2
                .status_plane
                .get("handshake_service")
                .map(|s| s.clone());
            assert_eq!(status, Some(ServiceStatus::Terminated));
        })
        .await;
    }

    #[tokio::test]
    async fn test_instance_isolation() {
        // This test verifies that two separate DaemonResources instances
        // do not share state, proving the removal of global pollution.
        let resources_a = create_test_resources();
        let resources_b = create_test_resources();

        resources_a
            .status_plane
            .insert("isolated_svc".to_string(), ServiceStatus::Healthy);
        resources_b
            .status_plane
            .insert("isolated_svc".to_string(), ServiceStatus::Initializing);

        let identity_a = create_test_identity("isolated_svc");
        let identity_b = create_test_identity("isolated_svc");

        let status_a = in_scope(identity_a, resources_a, || async { state() }).await;
        let status_b = in_scope(identity_b, resources_b, || async { state() }).await;

        assert_eq!(status_a, ServiceStatus::Healthy);
        assert_eq!(status_b, ServiceStatus::Initializing);
    }

    #[tokio::test]
    async fn test_is_shutdown_handshake_optimization() {
        // Verify that is_shutdown only performs the handshake once
        let resources = create_test_resources();
        resources
            .status_plane
            .insert("opt_svc".to_string(), ServiceStatus::Initializing);

        let identity = create_test_identity("opt_svc");
        let resources_clone = resources.clone();

        in_scope(identity, resources, || async move {
            // First call should trigger handshake
            assert!(!is_shutdown());

            // Status should now be Healthy
            let status = resources_clone
                .status_plane
                .get("opt_svc")
                .map(|s| s.clone());
            assert_eq!(status, Some(ServiceStatus::Healthy));

            // Revert status to Initializing to prove the flag prevents re-handshake
            resources_clone
                .status_plane
                .insert("opt_svc".to_string(), ServiceStatus::Initializing);

            // Second call should NOT re-handshake (flag is set)
            assert!(!is_shutdown());

            // Status should remain Initializing because handshake was skipped
            let status2 = resources_clone
                .status_plane
                .get("opt_svc")
                .map(|s| s.clone());
            assert_eq!(status2, Some(ServiceStatus::Initializing));
        })
        .await;
    }
}
