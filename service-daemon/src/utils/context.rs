use dashmap::DashMap;
use std::any::Any;
use std::sync::{Arc, LazyLock};
use tokio::task_local;
use tokio_util::sync::CancellationToken;

/// Represents the current lifecycle state of a service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceState {
    /// The service is starting for the first time in this process session.
    Starting,
    /// The service has been restarted after a configuration or dependency change and is ready to restore state.
    Restoring,
    /// The service is running normally.
    Running,
    /// A dependency changed, the service should save its state and prepare to exit.
    NeedReload,
    /// The service is being shut down.
    Stopping,
    /// The service is recovering from a previous crash (panic or error).
    /// Contains the error message from the previous generation.
    Recovering(String),
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

/// Global shelf for cross-generational state persistence (managed values).
/// Structure: DashMap<ServiceName, DashMap<Key, Value>>
pub static GLOBAL_SHELF: LazyLock<DashMap<String, DashMap<String, Box<dyn Any + Send + Sync>>>> =
    LazyLock::new(DashMap::new);

/// Stores the current state for each service.
pub static SERVICE_STATE_STORE: LazyLock<DashMap<String, ServiceState>> =
    LazyLock::new(DashMap::new);

/// Signals for services to reload.
pub static RELOAD_SIGNALS: LazyLock<DashMap<String, Arc<tokio::sync::Notify>>> =
    LazyLock::new(DashMap::new);

/// Returns the current state of the calling service.
pub fn state() -> ServiceState {
    if let Ok(id) = CURRENT_SERVICE.try_with(|id| id.clone()) {
        if id.cancellation_token.is_cancelled() {
            return ServiceState::Stopping;
        }

        // If the reload token is cancelled (dependency change), we force NeedReload status
        // unless the supervisor already set something else (like Stopping).
        if id.reload_token.is_cancelled() {
            let current = SERVICE_STATE_STORE.get(&id.name).map(|s| s.value().clone());
            match current {
                Some(ServiceState::Stopping) => return ServiceState::Stopping,
                _ => return ServiceState::NeedReload,
            }
        }

        SERVICE_STATE_STORE
            .get(&id.name)
            .map(|s| s.value().clone())
            .unwrap_or(ServiceState::Starting)
    } else {
        ServiceState::Starting
    }
}

/// Signals that the service has completed its current state (e.g. initialization).
/// This will advance the service state to the next logical step.
pub fn done() {
    if let Ok(id) = CURRENT_SERVICE.try_with(|id| id.clone()) {
        let current_state = SERVICE_STATE_STORE
            .get(&id.name)
            .map(|s| s.value().clone())
            .unwrap_or(ServiceState::Starting);

        let next_state = match &current_state {
            ServiceState::Starting | ServiceState::Restoring | ServiceState::Recovering(_) => {
                ServiceState::Running
            }
            _ => current_state.clone(),
        };

        SERVICE_STATE_STORE.insert(id.name.clone(), next_state.clone());
        tracing::info!(
            "Service '{}' signalled done() (Transition: {:?} -> {:?})",
            id.name,
            current_state,
            next_state
        );
    }
}

/// Shelves a managed value to the daemon. This value will survive service reloads.
pub async fn shelve<T: Any + Send + Sync>(key: &str, data: T) {
    if let Ok(name) = CURRENT_SERVICE.try_with(|id| id.name.clone()) {
        let entry = GLOBAL_SHELF.entry(name).or_insert_with(DashMap::new);
        entry.insert(key.to_string(), Box::new(data));
    }
}

/// Retrieves a shelved managed value previously submitted by this service.
pub async fn unshelve<T: Any + Send + Sync>(key: &str) -> Option<T> {
    if let Ok(name) = CURRENT_SERVICE.try_with(|id| id.name.clone()) {
        if let Some(entry) = GLOBAL_SHELF.get(&name) {
            return entry
                .remove(key)
                .map(|(_, val)| val.downcast::<T>().ok().map(|b| *b))
                .flatten();
        }
    }
    None
}

/// Checks if the current service or the daemon has been cancelled, or if a reload is requested.
pub fn is_shutdown() -> bool {
    let s = state();
    matches!(s, ServiceState::Stopping | ServiceState::NeedReload)
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

        SERVICE_STATE_STORE.insert("state_service".to_string(), ServiceState::NeedReload);

        CURRENT_SERVICE
            .scope(identity, async {
                assert!(matches!(state(), ServiceState::NeedReload));
            })
            .await;
    }
}
