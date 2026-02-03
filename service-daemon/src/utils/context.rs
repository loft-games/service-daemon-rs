use dashmap::DashMap;
use std::any::Any;
use std::sync::Arc;
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
#[derive(Clone)]
pub struct ServiceIdentity {
    pub name: String,
    pub reload_signal: Arc<tokio::sync::Notify>,
    pub cancellation_token: CancellationToken,
    pub reload_token: CancellationToken,
    /// A reference to the owning daemon's shared resources.
    pub resources: DaemonResources,
}

task_local! {
    pub static CURRENT_SERVICE: ServiceIdentity;
}

/// Returns the current lifecycle status of the calling service.
pub fn state() -> ServiceStatus {
    if let Ok(id) = CURRENT_SERVICE.try_with(|id| id.clone()) {
        if id.cancellation_token.is_cancelled() {
            return ServiceStatus::ShuttingDown;
        }

        // If the reload token is cancelled (dependency change), we force NeedReload status
        // unless the supervisor already set something else (like ShuttingDown).
        if id.reload_token.is_cancelled() {
            let current = id
                .resources
                .status_plane
                .get(&id.name)
                .map(|s| s.value().clone());
            match current {
                Some(ServiceStatus::ShuttingDown) => return ServiceStatus::ShuttingDown,
                _ => return ServiceStatus::NeedReload,
            }
        }

        id.resources
            .status_plane
            .get(&id.name)
            .map(|s| s.value().clone())
            .unwrap_or(ServiceStatus::Initializing)
    } else {
        ServiceStatus::Initializing
    }
}

/// Signals that the service has completed its current state (e.g. initialization or cleanup).
/// This will advance the service status to the next logical step based on the handshake protocol:
/// - `Initializing | Restoring | Recovering` -> `Healthy` (service is now ready).
/// - `NeedReload | ShuttingDown` -> `Terminated` (service is ready for collection).
/// - Otherwise, no-op.
pub fn done() {
    if let Ok(id) = CURRENT_SERVICE.try_with(|id| id.clone()) {
        let current_status = id
            .resources
            .status_plane
            .get(&id.name)
            .map(|s| s.value().clone())
            .unwrap_or(ServiceStatus::Initializing);

        let next_status = match &current_status {
            ServiceStatus::Initializing
            | ServiceStatus::Restoring
            | ServiceStatus::Recovering(_) => ServiceStatus::Healthy,
            ServiceStatus::NeedReload | ServiceStatus::ShuttingDown => ServiceStatus::Terminated,
            _ => current_status.clone(), // No-op for Healthy and Terminated
        };

        if next_status != current_status {
            id.resources
                .status_plane
                .insert(id.name.clone(), next_status.clone());
            id.resources.status_changed.notify_waiters();
            tracing::info!(
                "Service '{}' signalled done() (Transition: {:?} -> {:?})",
                id.name,
                current_status,
                next_status
            );
        }
    }
}

/// Shelves a managed value to the daemon. This value will survive service reloads and crashes.
/// The value is stored in a service-isolated bucket based on the calling service's identity.
pub async fn shelve<T: Any + Send + Sync>(key: &str, data: T) {
    if let Ok(id) = CURRENT_SERVICE.try_with(|id| id.clone()) {
        let entry = id.resources.shelf.entry(id.name.clone()).or_default();
        entry.insert(key.to_string(), Box::new(data));
    }
}

/// Retrieves a shelved managed value previously submitted by this service.
/// The value is retrieved from the service's isolated bucket.
pub async fn unshelve<T: Any + Send + Sync>(key: &str) -> Option<T> {
    if let Ok(id) = CURRENT_SERVICE.try_with(|id| id.clone())
        && let Some(entry) = id.resources.shelf.get(&id.name)
    {
        return entry
            .remove(key)
            .and_then(|(_, val)| val.downcast::<T>().ok().map(|b| *b));
    }
    None
}

/// Performs an implicit handshake if the service is still in a "Starting" phase.
/// This enables a smooth "growth curve" for minimalist services that just use
/// `while !is_shutdown()` without needing to call `done()` explicitly.
fn implicit_handshake() {
    if let Ok(id) = CURRENT_SERVICE.try_with(|id| id.clone())
        && let Some(status) = id.resources.status_plane.get(&id.name)
        && matches!(
            status.value(),
            ServiceStatus::Initializing | ServiceStatus::Restoring | ServiceStatus::Recovering(_)
        )
    {
        // Auto-transition to Healthy
        drop(status); // Release the read lock before inserting
        id.resources
            .status_plane
            .insert(id.name.clone(), ServiceStatus::Healthy);
        id.resources.status_changed.notify_waiters();
        tracing::debug!(
            "Service '{}' implicitly transitioned to Healthy (via lifecycle utility)",
            id.name
        );
    }
}

/// Checks if the current service or the daemon has been signaled to stop or reload.
/// Returns `true` for any "descending" status (NeedReload, ShuttingDown, Terminated).
///
/// **Note**: If the service is still in a "Starting" phase, this function will
/// implicitly transition it to `Healthy` status.
pub fn is_shutdown() -> bool {
    implicit_handshake();
    let s = state();
    matches!(
        s,
        ServiceStatus::ShuttingDown | ServiceStatus::NeedReload | ServiceStatus::Terminated
    )
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

    #[tokio::test]
    async fn test_shelve_unshelve() {
        let resources = create_test_resources();
        let identity = ServiceIdentity {
            name: "test_service".to_string(),
            reload_signal: Arc::new(tokio::sync::Notify::new()),
            cancellation_token: CancellationToken::new(),
            reload_token: CancellationToken::new(),
            resources,
        };

        CURRENT_SERVICE
            .scope(identity, async {
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
        let identity = ServiceIdentity {
            name: "state_service".to_string(),
            reload_signal: Arc::new(tokio::sync::Notify::new()),
            cancellation_token: CancellationToken::new(),
            reload_token: CancellationToken::new(),
            resources: resources.clone(),
        };

        resources
            .status_plane
            .insert("state_service".to_string(), ServiceStatus::NeedReload);

        CURRENT_SERVICE
            .scope(identity, async {
                assert!(matches!(state(), ServiceStatus::NeedReload));
            })
            .await;
    }

    #[tokio::test]
    async fn test_handshake_protocol() {
        let resources = create_test_resources();
        let identity = ServiceIdentity {
            name: "handshake_service".to_string(),
            reload_signal: Arc::new(tokio::sync::Notify::new()),
            cancellation_token: CancellationToken::new(),
            reload_token: CancellationToken::new(),
            resources: resources.clone(),
        };

        // Start in Initializing
        resources
            .status_plane
            .insert("handshake_service".to_string(), ServiceStatus::Initializing);

        CURRENT_SERVICE
            .scope(identity.clone(), async {
                // After done(), status should become Healthy
                done();
                let status = resources
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

        CURRENT_SERVICE
            .scope(identity.clone(), async {
                // After done(), status should become Terminated
                done();
                let status = resources
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

        let identity_a = ServiceIdentity {
            name: "isolated_svc".to_string(),
            reload_signal: Arc::new(tokio::sync::Notify::new()),
            cancellation_token: CancellationToken::new(),
            reload_token: CancellationToken::new(),
            resources: resources_a.clone(),
        };

        let identity_b = ServiceIdentity {
            name: "isolated_svc".to_string(),
            reload_signal: Arc::new(tokio::sync::Notify::new()),
            cancellation_token: CancellationToken::new(),
            reload_token: CancellationToken::new(),
            resources: resources_b.clone(),
        };

        let status_a = CURRENT_SERVICE.scope(identity_a, async { state() }).await;
        let status_b = CURRENT_SERVICE.scope(identity_b, async { state() }).await;

        assert_eq!(status_a, ServiceStatus::Healthy);
        assert_eq!(status_b, ServiceStatus::Initializing);
    }
}
