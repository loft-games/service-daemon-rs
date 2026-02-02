use dashmap::DashMap;
use std::any::Any;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tokio::task_local;
use tokio_util::sync::CancellationToken;

use crate::models::ServiceStatus;

/// Internal identity of a service used to link task-local calls to the daemon's management.
#[derive(Clone)]
pub struct ServiceIdentity {
    pub name: String,
    pub reload_signal: Arc<tokio::sync::Notify>,
    pub cancellation_token: CancellationToken,
    pub reload_token: CancellationToken,
}

task_local! {
    pub static CURRENT_SERVICE: ServiceIdentity;
}

type ShelfValue = Box<dyn Any + Send + Sync>;
type ServiceShelf = DashMap<String, ShelfValue>;
type GlobalShelfMapping = DashMap<String, ServiceShelf>;

/// Global shelf for cross-generational state persistence (managed values).
/// Structure: DashMap<ServiceName, DashMap<Key, Value>>
pub static GLOBAL_SHELF: LazyLock<GlobalShelfMapping> = LazyLock::new(DashMap::new);

/// The unified Status Plane: stores the current lifecycle status for each service.
pub static GLOBAL_STATUS_PLANE: LazyLock<DashMap<String, ServiceStatus>> =
    LazyLock::new(DashMap::new);

/// Signals for services to reload.
pub static RELOAD_SIGNALS: LazyLock<DashMap<String, Arc<tokio::sync::Notify>>> =
    LazyLock::new(DashMap::new);

/// Global notification for any status change in the STATUS_PLANE.
pub static STATUS_CHANGED: LazyLock<Arc<tokio::sync::Notify>> =
    LazyLock::new(|| Arc::new(tokio::sync::Notify::new()));

/// Returns the current lifecycle status of the calling service.
pub fn state() -> ServiceStatus {
    if let Ok(id) = CURRENT_SERVICE.try_with(|id| id.clone()) {
        if id.cancellation_token.is_cancelled() {
            return ServiceStatus::ShuttingDown;
        }

        // If the reload token is cancelled (dependency change), we force NeedReload status
        // unless the supervisor already set something else (like ShuttingDown).
        if id.reload_token.is_cancelled() {
            let current = GLOBAL_STATUS_PLANE.get(&id.name).map(|s| s.value().clone());
            match current {
                Some(ServiceStatus::ShuttingDown) => return ServiceStatus::ShuttingDown,
                _ => return ServiceStatus::NeedReload,
            }
        }

        GLOBAL_STATUS_PLANE
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
        let current_status = GLOBAL_STATUS_PLANE
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
            GLOBAL_STATUS_PLANE.insert(id.name.clone(), next_status.clone());
            STATUS_CHANGED.notify_waiters();
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
    if let Ok(name) = CURRENT_SERVICE.try_with(|id| id.name.clone()) {
        let entry = GLOBAL_SHELF.entry(name).or_default();
        entry.insert(key.to_string(), Box::new(data));
    }
}

/// Retrieves a shelved managed value previously submitted by this service.
/// The value is retrieved from the service's isolated bucket.
pub async fn unshelve<T: Any + Send + Sync>(key: &str) -> Option<T> {
    if let Ok(name) = CURRENT_SERVICE.try_with(|id| id.name.clone())
        && let Some(entry) = GLOBAL_SHELF.get(&name)
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
        && let Some(status) = GLOBAL_STATUS_PLANE.get(&id.name)
        && matches!(
            status.value(),
            ServiceStatus::Initializing | ServiceStatus::Restoring | ServiceStatus::Recovering(_)
        )
    {
        // Auto-transition to Healthy
        drop(status); // Release the read lock before inserting
        GLOBAL_STATUS_PLANE.insert(id.name.clone(), ServiceStatus::Healthy);
        STATUS_CHANGED.notify_waiters();
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

    #[tokio::test]
    async fn test_shelve_unshelve() {
        let identity = ServiceIdentity {
            name: "test_service".to_string(),
            reload_signal: Arc::new(tokio::sync::Notify::new()),
            cancellation_token: CancellationToken::new(),
            reload_token: CancellationToken::new(),
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
        let identity = ServiceIdentity {
            name: "state_service".to_string(),
            reload_signal: Arc::new(tokio::sync::Notify::new()),
            cancellation_token: CancellationToken::new(),
            reload_token: CancellationToken::new(),
        };

        GLOBAL_STATUS_PLANE.insert("state_service".to_string(), ServiceStatus::NeedReload);

        CURRENT_SERVICE
            .scope(identity, async {
                assert!(matches!(state(), ServiceStatus::NeedReload));
            })
            .await;
    }

    #[tokio::test]
    async fn test_handshake_protocol() {
        let identity = ServiceIdentity {
            name: "handshake_service".to_string(),
            reload_signal: Arc::new(tokio::sync::Notify::new()),
            cancellation_token: CancellationToken::new(),
            reload_token: CancellationToken::new(),
        };

        // Start in Initializing
        GLOBAL_STATUS_PLANE.insert("handshake_service".to_string(), ServiceStatus::Initializing);

        CURRENT_SERVICE
            .scope(identity.clone(), async {
                // After done(), status should become Healthy
                done();
                let status = GLOBAL_STATUS_PLANE
                    .get("handshake_service")
                    .map(|s| s.clone());
                assert_eq!(status, Some(ServiceStatus::Healthy));
            })
            .await;

        // Now test the descending phase
        GLOBAL_STATUS_PLANE.insert("handshake_service".to_string(), ServiceStatus::NeedReload);

        CURRENT_SERVICE
            .scope(identity.clone(), async {
                // After done(), status should become Terminated
                done();
                let status = GLOBAL_STATUS_PLANE
                    .get("handshake_service")
                    .map(|s| s.clone());
                assert_eq!(status, Some(ServiceStatus::Terminated));
            })
            .await;
    }
}
