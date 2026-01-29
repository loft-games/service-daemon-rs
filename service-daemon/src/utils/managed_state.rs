use crate::models::MUTABILITY_REGISTRY;
use std::sync::Arc;
use tokio::sync::{Mutex, OnceCell, RwLock};

/// Manages intelligent promotion and synchronization for shared state.
/// T must be Clone to support snapshot-based reading when promoted to managed state.
pub struct StateManager<T: 'static + Send + Sync + Clone> {
    lock: OnceCell<Arc<RwLock<T>>>,
    mutex: OnceCell<Arc<Mutex<T>>>,
    snapshot: OnceCell<Arc<T>>,
}

impl<T: 'static + Send + Sync + Clone> StateManager<T> {
    pub const fn new() -> Self {
        Self {
            lock: OnceCell::const_new(),
            mutex: OnceCell::const_new(),
            snapshot: OnceCell::const_new(),
        }
    }

    /// Checks if the type T is marked as mutable anywhere in the system.
    pub fn is_promoted(key: &'static str) -> bool {
        MUTABILITY_REGISTRY.iter().any(|m| m.key == key)
    }

    /// Resolves as a live RwLock.
    pub async fn resolve_rwlock<F, Fut>(&self, init: F) -> Arc<RwLock<T>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Arc<T>> + Send,
    {
        self.lock
            .get_or_init(|| async {
                let initial_arc = init().await;
                let val = (*initial_arc).clone();
                Arc::new(RwLock::new(val))
            })
            .await
            .clone()
    }

    /// Resolves as a live Mutex.
    pub async fn resolve_mutex<F, Fut>(&self, init: F) -> Arc<Mutex<T>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Arc<T>> + Send,
    {
        self.mutex
            .get_or_init(|| async {
                let initial_arc = init().await;
                let val = (*initial_arc).clone();
                Arc::new(Mutex::new(val))
            })
            .await
            .clone()
    }

    /// Resolves as a snapshot Arc<T>.
    /// If promoted, it attempts to provide a consistent snapshot by reading the RwLock.
    /// Otherwise, it uses the fast-path immutable singleton.
    pub async fn resolve_snapshot<F, Fut>(&self, key: &'static str, init: F) -> Arc<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Arc<T>> + Send,
    {
        if Self::is_promoted(key) {
            // Managed Path: Resolve the RwLock and take a snapshot
            let rw = self.resolve_rwlock(init).await;
            let val = rw.read().await.clone();
            Arc::new(val)
        } else {
            // Fast Path: Resolve the immutable singleton
            self.snapshot.get_or_init(init).await.clone()
        }
    }
}
