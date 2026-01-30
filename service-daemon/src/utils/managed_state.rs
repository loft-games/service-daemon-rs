use crate::models::MUTABILITY_REGISTRY;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use tokio::sync::{
    Mutex as TokioMutex, MutexGuard as TokioMutexGuard, OnceCell, RwLock as TokioRwLock,
    RwLockReadGuard as TokioRwLockReadGuard, RwLockWriteGuard as TokioRwLockWriteGuard,
};

/// Manages intelligent promotion and synchronization for shared state.
///
/// `StateManager` handles the transition between immutable singletons and
/// mutable tracked state. It provides a "Macro Illusion" that allows services
/// to interact with state as if it were a standard `RwLock` or `Mutex`, while
/// internally managing snapshots and change notifications for the `Watch` trigger system.
///
/// T must be `Clone` to support snapshot-based reading when the state is promoted
/// to managed (mutable) state.
pub struct StateManager<T: 'static + Send + Sync + Clone> {
    lock: OnceCell<Arc<TrackedRwLock<T>>>,
    mutex: OnceCell<Arc<TrackedMutex<T>>>,
    snapshot: OnceCell<Arc<T>>,
    change_notify: OnceCell<Arc<tokio::sync::Notify>>,
}

impl<T: 'static + Send + Sync + Clone> StateManager<T> {
    pub const fn new() -> Self {
        Self {
            lock: OnceCell::const_new(),
            mutex: OnceCell::const_new(),
            snapshot: OnceCell::const_new(),
            change_notify: OnceCell::const_new(),
        }
    }

    /// Internal helper to get or initialize the shared notification handle.
    async fn get_notify(&self) -> Arc<tokio::sync::Notify> {
        self.change_notify
            .get_or_init(|| async { Arc::new(tokio::sync::Notify::new()) })
            .await
            .clone()
    }

    /// Checks if the type T is marked as mutable anywhere in the system.
    pub fn is_promoted(key: &'static str) -> bool {
        MUTABILITY_REGISTRY.iter().any(|m| m.key == key)
    }

    /// Resolves as a tracked RwLock.
    pub async fn resolve_rwlock<F, Fut>(&self, init: F) -> Arc<TrackedRwLock<T>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Arc<T>> + Send,
    {
        self.lock
            .get_or_init(|| async {
                let initial_arc = init().await;
                let val = (*initial_arc).clone();
                let notify = self.get_notify().await;
                Arc::new(TrackedRwLock {
                    inner: TokioRwLock::new(val),
                    notify,
                })
            })
            .await
            .clone()
    }

    /// Resolves as a tracked Mutex.
    pub async fn resolve_mutex<F, Fut>(&self, init: F) -> Arc<TrackedMutex<T>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Arc<T>> + Send,
    {
        self.mutex
            .get_or_init(|| async {
                let initial_arc = init().await;
                let val = (*initial_arc).clone();
                let notify = self.get_notify().await;
                Arc::new(TrackedMutex {
                    inner: TokioMutex::new(val),
                    notify,
                })
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

    /// Returns a future that resolves when the state is modified.
    pub async fn changed(&self) {
        let notify = self.get_notify().await;
        notify.notified().await;
    }
}

/// An asynchronous reader-writer lock with automatic change tracking.
///
/// This type is a tracked version of [tokio::sync::RwLock]. It automatically
/// notifies the `ServiceDaemon` state management system whenever a write lock
/// is released, enabling the [Watch](service_daemon::trigger) trigger to fire.
///
/// For detailed behavioral documentation, see the official [tokio::sync::RwLock].
#[doc(alias = "tokio::sync::RwLock")]
pub struct TrackedRwLock<T> {
    inner: TokioRwLock<T>,
    notify: Arc<tokio::sync::Notify>,
}

impl<T> TrackedRwLock<T> {
    /// Locks this `RwLock` with shared read access.
    ///
    /// See also [tokio::sync::RwLock::read].
    pub async fn read(&self) -> TokioRwLockReadGuard<'_, T> {
        self.inner.read().await
    }

    /// Locks this `RwLock` with exclusive write access.
    ///
    /// When the returned [TrackedWriteGuard] is dropped, any `Watch` triggers
    /// listening to this state will be notified.
    ///
    /// See also [tokio::sync::RwLock::write].
    pub async fn write(&self) -> TrackedWriteGuard<'_, T> {
        TrackedWriteGuard {
            inner: self.inner.write().await,
            notify: self.notify.clone(),
        }
    }
}

/// RAII structure used to release the exclusive write access of a `RwLock`
/// and notify state observers.
pub struct TrackedWriteGuard<'a, T> {
    inner: TokioRwLockWriteGuard<'a, T>,
    notify: Arc<tokio::sync::Notify>,
}

impl<T> Deref for TrackedWriteGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T> DerefMut for TrackedWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<T> Drop for TrackedWriteGuard<'_, T> {
    fn drop(&mut self) {
        self.notify.notify_waiters();
    }
}

/// An asynchronous mutual exclusion primitive with automatic change tracking.
///
/// This type is a tracked version of [tokio::sync::Mutex]. It automatically
/// notifies the `ServiceDaemon` state management system whenever the lock
/// is released, enabling the [Watch](service_daemon::trigger) trigger to fire.
///
/// For detailed behavioral documentation, see the official [tokio::sync::Mutex].
#[doc(alias = "tokio::sync::Mutex")]
pub struct TrackedMutex<T> {
    inner: TokioMutex<T>,
    notify: Arc<tokio::sync::Notify>,
}

impl<T> TrackedMutex<T> {
    /// Locks this `Mutex`, causing the current task to yield until the lock has been acquired.
    ///
    /// When the returned [TrackedMutexGuard] is dropped, any `Watch` triggers
    /// listening to this state will be notified.
    ///
    /// See also [tokio::sync::Mutex::lock].
    pub async fn lock(&self) -> TrackedMutexGuard<'_, T> {
        TrackedMutexGuard {
            inner: self.inner.lock().await,
            notify: self.notify.clone(),
        }
    }
}

/// RAII structure used to release the exclusive lock of a `Mutex`
/// and notify state observers.
pub struct TrackedMutexGuard<'a, T> {
    inner: TokioMutexGuard<'a, T>,
    notify: Arc<tokio::sync::Notify>,
}

impl<T> Deref for TrackedMutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T> DerefMut for TrackedMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<T> Drop for TrackedMutexGuard<'_, T> {
    fn drop(&mut self) {
        self.notify.notify_waiters();
    }
}

// Aliases for the macro "illusion"
pub use TrackedMutex as Mutex;
pub use TrackedRwLock as RwLock;

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::Notify;

    #[tokio::test]
    async fn test_tracked_rwlock_notification() {
        let notify = Arc::new(Notify::new());
        let lock = TrackedRwLock {
            inner: TokioRwLock::new(0),
            notify: notify.clone(),
        };

        // Read doesn't notify
        {
            let _guard = lock.read().await;
            let wait = notify.notified();
            drop(_guard);
            assert!(
                tokio::select! {
                    _ = wait => true,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => false,
                } == false
            );
        }

        // Write notifies on drop
        {
            let _guard = lock.write().await;
            let wait = notify.notified();
            drop(_guard);
            assert!(tokio::select! {
                _ = wait => true,
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => false,
            });
        }
    }

    #[tokio::test]
    async fn test_tracked_mutex_notification() {
        let notify = Arc::new(Notify::new());
        let lock = TrackedMutex {
            inner: TokioMutex::new(0),
            notify: notify.clone(),
        };

        // Lock notifies on drop
        {
            let _guard = lock.lock().await;
            let wait = notify.notified();
            drop(_guard);
            assert!(tokio::select! {
                _ = wait => true,
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => false,
            });
        }
    }
}
