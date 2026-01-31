use dashmap::DashMap;
use std::any::Any;
use std::sync::Arc;
use tokio::sync::OnceCell;
use tokio::task_local;
use tokio_util::sync::CancellationToken;

/// Represents the current lifecycle state of a service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceState {
    /// The service is starting for the first time in this process session.
    Starting,
    /// The service has been restarted after a configuration or dependency change.
    Reloading,
    /// The service is recovering from a previous crash (panic or error).
    /// Contains the error message from the previous generation.
    Recovering(String),
    /// The service is running normally.
    Running,
    /// The service is being shut down.
    Stopping,
}

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

/// Global shelf for cross-generational state persistence.
pub static GLOBAL_SHELF: OnceCell<DashMap<String, Box<dyn Any + Send + Sync>>> =
    OnceCell::const_new();
/// Stores the current state for each service.
pub static SERVICE_STATE_STORE: OnceCell<DashMap<String, ServiceState>> = OnceCell::const_new();
/// Signals for services to reload.
pub static RELOAD_SIGNALS: OnceCell<DashMap<String, Arc<tokio::sync::Notify>>> =
    OnceCell::const_new();

/// Returns the current state of the calling service.
pub async fn state() -> ServiceState {
    if let Ok(id) = CURRENT_SERVICE.try_with(|id| id.clone()) {
        if id.cancellation_token.is_cancelled() {
            return ServiceState::Stopping;
        }
        if id.reload_token.is_cancelled() {
            return ServiceState::Reloading;
        }

        SERVICE_STATE_STORE
            .get_or_init(|| async { DashMap::new() })
            .await
            .get(&id.name)
            .map(|s| s.value().clone())
            .unwrap_or(ServiceState::Starting)
    } else {
        ServiceState::Starting
    }
}

/// Signals that the service has completed its current state transition (e.g. initialization).
/// This will set the service state to `Running` in the global store.
pub async fn done() {
    if let Ok(name) = CURRENT_SERVICE.try_with(|id| id.name.clone()) {
        SERVICE_STATE_STORE
            .get_or_init(|| async { DashMap::new() })
            .await
            .insert(name.clone(), ServiceState::Running);
        tracing::info!("Service '{}' signalled done()", name);
    }
}

/// Shelves a piece of state for the next generation of this service.
pub async fn shelve<T: Any + Send + Sync>(data: T) {
    if let Ok(id) = CURRENT_SERVICE.try_with(|id| id.name.clone()) {
        GLOBAL_SHELF
            .get_or_init(|| async { DashMap::new() })
            .await
            .insert(id, Box::new(data));
    }
}

/// Retrieves the shelved state from the previous generation of this service.
pub async fn unshelve<T: Any + Send + Sync>() -> Option<T> {
    if let Ok(name) = CURRENT_SERVICE.try_with(|id| id.name.clone()) {
        GLOBAL_SHELF
            .get_or_init(|| async { DashMap::new() })
            .await
            .remove(&name)
            .and_then(|(_, val)| val.downcast::<T>().ok().map(|b| *b))
    } else {
        None
    }
}

/// Waits for a reload signal specific to this service.
pub async fn wait_for_reload() {
    if let Ok(id) = CURRENT_SERVICE.try_with(|id| id.reload_signal.clone()) {
        id.notified().await;
    } else {
        futures::future::pending::<()>().await;
    }
}

/// Returns the cancellation token for the current service.
pub fn token() -> CancellationToken {
    CURRENT_SERVICE
        .try_with(|id| id.cancellation_token.clone())
        .unwrap_or_default()
}

/// Checks if the current service or the daemon has been cancelled.
pub fn is_shutdown() -> bool {
    token().is_cancelled()
}

/// Returns a future that resolves when the current service or daemon is shut down.
pub async fn wait_for_shutdown() {
    token().cancelled().await;
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
                shelve(42i32).await;
                let val: Option<i32> = unshelve().await;
                assert_eq!(val, Some(42));

                // Verify it's removed after unshelve
                let val2: Option<i32> = unshelve().await;
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

        SERVICE_STATE_STORE
            .get_or_init(|| async { DashMap::new() })
            .await
            .insert("state_service".to_string(), ServiceState::Reloading);

        CURRENT_SERVICE
            .scope(identity, async {
                assert!(matches!(state().await, ServiceState::Reloading));
            })
            .await;
    }
}
