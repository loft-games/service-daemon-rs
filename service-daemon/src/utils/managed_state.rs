use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use tokio::sync::{
    OnceCell, RwLock as TokioRwLock, RwLockReadGuard as TokioRwLockReadGuard,
    RwLockWriteGuard as TokioRwLockWriteGuard, watch,
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
    snapshot: OnceCell<Arc<T>>,
    watch_rx: OnceCell<watch::Receiver<Arc<T>>>,
    change_notify: OnceCell<Arc<tokio::sync::Notify>>,
}

impl<T: 'static + Send + Sync + Clone> StateManager<T> {
    pub const fn new() -> Self {
        Self {
            lock: OnceCell::const_new(),
            snapshot: OnceCell::const_new(),
            watch_rx: OnceCell::const_new(),
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

    /// Resolves as a tracked RwLock.
    pub async fn resolve_rwlock<F, Fut>(&self, init: F) -> Arc<TrackedRwLock<T>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Arc<T>> + Send,
    {
        self.lock
            .get_or_init(|| async {
                // If we already have a snapshot, use it to seed the lock to avoid double-init
                let initial_arc = if let Some(sn) = self.snapshot.get() {
                    sn.clone()
                } else {
                    init().await
                };

                let val = (*initial_arc).clone();
                let notify = self.get_notify().await;
                let (tx, rx) = watch::channel(initial_arc);

                // Ensure watch_rx is also populated
                let _ = self.watch_rx.set(rx);

                Arc::new(TrackedRwLock {
                    inner: TokioRwLock::new(val),
                    notify,
                    watch_tx: tx,
                })
            })
            .await
            .clone()
    }

    /// Resolves as a tracked Mutex (backed by the same underlying RwLock).
    pub async fn resolve_mutex<F, Fut>(&self, init: F) -> Arc<TrackedMutex<T>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Arc<T>> + Send,
    {
        let lock = self.resolve_rwlock(init).await;
        Arc::new(TrackedMutex { inner: lock })
    }

    /// Resolves as a snapshot Arc<T>.
    /// Provides "Zero Lockdown" reads - never blocks even if a writer is holding the lock.
    pub async fn resolve_snapshot<F, Fut>(&self, init: F) -> Arc<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Arc<T>> + Send,
    {
        // 1. Dynamic Check: If lock is already initialized, we MUST use the managed path
        if let Some(rx) = self.watch_rx.get() {
            return rx.borrow().clone();
        }

        // 2. Fast Path: Plain immutable singleton
        self.snapshot.get_or_init(init).await.clone()
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
pub struct TrackedRwLock<T: Clone> {
    inner: TokioRwLock<T>,
    notify: Arc<tokio::sync::Notify>,
    watch_tx: watch::Sender<Arc<T>>,
}

impl<T: Clone> TrackedRwLock<T> {
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
            watch_tx: &self.watch_tx,
        }
    }
}

/// RAII structure used to release the exclusive write access of a `RwLock`
/// and notify state observers.
pub struct TrackedWriteGuard<'a, T: Clone> {
    inner: TokioRwLockWriteGuard<'a, T>,
    notify: Arc<tokio::sync::Notify>,
    watch_tx: &'a watch::Sender<Arc<T>>,
}

impl<T: Clone> Deref for TrackedWriteGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T: Clone> DerefMut for TrackedWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<T: Clone> Drop for TrackedWriteGuard<'_, T> {
    fn drop(&mut self) {
        // 1. Notify Watch triggers
        self.notify.notify_waiters();
        // 2. Update non-blocking watch channel
        let new_val = (*self.inner).clone();
        self.watch_tx.send_replace(Arc::new(new_val));
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
pub struct TrackedMutex<T: Clone> {
    inner: Arc<TrackedRwLock<T>>,
}

impl<T: Clone> TrackedMutex<T> {
    /// Locks this `Mutex`, causing the current task to yield until the lock has been acquired.
    ///
    /// When the returned [TrackedMutexGuard] is dropped, any `Watch` triggers
    /// listening to this state will be notified.
    ///
    /// See also [tokio::sync::Mutex::lock].
    pub async fn lock(&self) -> TrackedMutexGuard<'_, T> {
        TrackedMutexGuard {
            inner: self.inner.write().await,
        }
    }
}

/// RAII structure used to release the exclusive lock of a `Mutex`
/// and notify state observers.
pub struct TrackedMutexGuard<'a, T: Clone> {
    inner: TrackedWriteGuard<'a, T>,
}

impl<T: Clone> Deref for TrackedMutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T: Clone> DerefMut for TrackedMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
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
        let (tx, _rx) = watch::channel(Arc::new(0));
        let lock = TrackedRwLock {
            inner: TokioRwLock::new(0),
            notify: notify.clone(),
            watch_tx: tx,
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
        let (tx, _rx) = watch::channel(Arc::new(0));
        let rw = Arc::new(TrackedRwLock {
            inner: TokioRwLock::new(0),
            notify: notify.clone(),
            watch_tx: tx,
        });
        let lock = TrackedMutex { inner: rw };

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
