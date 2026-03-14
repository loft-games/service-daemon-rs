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
    snapshot_cache: OnceCell<Arc<T>>,
    watch_rx: OnceCell<watch::Receiver<Arc<T>>>,
    change_notify: OnceCell<Arc<tokio::sync::Notify>>,
}

impl<T: 'static + Send + Sync + Clone> Default for StateManager<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: 'static + Send + Sync + Clone> StateManager<T> {
    pub const fn new() -> Self {
        Self {
            lock: OnceCell::const_new(),
            snapshot_cache: OnceCell::const_new(),
            watch_rx: OnceCell::const_new(),
            change_notify: OnceCell::const_new(),
        }
    }

    /// Create a new StateManager with an initial value.
    pub fn with_value(val: T) -> Self {
        let manager = Self::new();
        let arc = Arc::new(val);
        manager.snapshot_cache.set(arc).ok();
        manager
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
                let initial_arc = if let Some(sn) = self.snapshot_cache.get() {
                    sn.clone()
                } else {
                    init().await
                };

                let val = initial_arc.clone();
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

    /// Resolves as a snapshot `Arc<T>`.
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
        self.snapshot_cache.get_or_init(init).await.clone()
    }

    /// Resolves as the raw initialization result.
    pub async fn resolve_managed<F, Fut>(
        &self,
        init: F,
    ) -> std::result::Result<Arc<T>, crate::ProviderError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = std::result::Result<Arc<T>, crate::ProviderError>> + Send,
    {
        // 1. Dynamic Check: If lock is already initialized, we return the latest snapshot
        if let Some(rx) = self.watch_rx.get() {
            return Ok(rx.borrow().clone());
        }

        // 2. Fallible Logic
        init().await
    }

    /// Convenience method to get a snapshot. Panics if not initialized.
    ///
    /// # Panics
    /// Panics if neither `resolve_snapshot` nor `resolve_rwlock` has been
    /// called for this `StateManager` yet. This typically indicates a provider
    /// was accessed before the `ServiceDaemon` had a chance to initialize it.
    pub async fn snapshot(&self) -> Arc<T> {
        if let Some(rx) = self.watch_rx.get() {
            return rx.borrow().clone();
        }
        self.snapshot_cache
            .get()
            .expect(
                "StateManager::snapshot() called before initialization. \
                 Ensure the corresponding provider is registered and the \
                 ServiceDaemon has started before accessing this state.",
            )
            .clone()
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
    inner: TokioRwLock<Arc<T>>,
    notify: Arc<tokio::sync::Notify>,
    watch_tx: watch::Sender<Arc<T>>,
}

impl<T: Clone> TrackedRwLock<T> {
    /// Locks this `RwLock` with shared read access.
    ///
    /// See also [tokio::sync::RwLock::read].
    pub async fn read(&self) -> TrackedReadGuard<'_, T> {
        TrackedReadGuard {
            inner: self.inner.read().await,
        }
    }

    /// Locks this `RwLock` with exclusive write access.
    ///
    /// When the returned [TrackedWriteGuard] is dropped, any `Watch` triggers
    /// listening to this state will be notified **only if the data was actually
    /// mutated** (i.e., `DerefMut` was invoked). This prevents spurious wakeups
    /// when write locks are acquired but no modification occurs.
    ///
    /// See also [tokio::sync::RwLock::write].
    pub async fn write(&self) -> TrackedWriteGuard<'_, T> {
        TrackedWriteGuard {
            inner: self.inner.write().await,
            notify: self.notify.clone(),
            watch_tx: &self.watch_tx,
            is_committed: false,
            is_dirty: false,
        }
    }
}

/// RAII structure used to release the shared read access of a `RwLock`.
pub struct TrackedReadGuard<'a, T: Clone> {
    inner: TokioRwLockReadGuard<'a, Arc<T>>,
}

impl<T: Clone> Deref for TrackedReadGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// RAII structure used to release the exclusive write access of a `RwLock`
/// and notify state observers.
///
/// Tracks whether the data was actually mutated via `DerefMut` using an
/// internal `is_dirty` flag. On `Drop`, auto-commit only fires if the
/// guard is both dirty and not yet manually committed, preventing
/// spurious wakeups and unnecessary clones.
pub struct TrackedWriteGuard<'a, T: Clone> {
    inner: TokioRwLockWriteGuard<'a, Arc<T>>,
    notify: Arc<tokio::sync::Notify>,
    watch_tx: &'a watch::Sender<Arc<T>>,
    is_committed: bool,
    is_dirty: bool,
}

impl<'a, T: Clone> TrackedWriteGuard<'a, T> {
    /// Commits the current state to the snapshot channel and notifies listeners.
    /// This can be called multiple times during a single write lock.
    pub fn commit(&mut self) {
        let new_val = (*self.inner).clone();
        self.watch_tx.send_replace(new_val);
        self.notify.notify_waiters();
        self.is_committed = true;
        self.is_dirty = true;
    }

    /// Replaces the entire state with a new Arc and commits it.
    /// This is the "Total Zero-Copy" path.
    pub fn publish(&mut self, new_val: Arc<T>) {
        *self.inner = new_val.clone();
        self.watch_tx.send_replace(new_val);
        self.notify.notify_waiters();
        self.is_committed = true;
    }
}

impl<T: Clone> Deref for TrackedWriteGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T: Clone> DerefMut for TrackedWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.is_dirty = true;
        Arc::make_mut(&mut self.inner)
    }
}

impl<T: Clone> Drop for TrackedWriteGuard<'_, T> {
    fn drop(&mut self) {
        if self.is_dirty && !self.is_committed {
            // Automatically commit on drop only if data was actually mutated
            // (DerefMut was called) and not yet manually committed.
            // This prevents spurious wakeups and unnecessary clones when
            // a write lock is acquired but no modification occurs.
            let new_val = (*self.inner).clone();
            self.watch_tx.send_replace(new_val);
            self.notify.notify_waiters();
        }
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
    async fn test_state_manager_zero_copy() {
        let manager = StateManager::<i32>::with_value(10);

        // Initial snapshot
        let snap1 = manager.snapshot().await;
        assert_eq!(*snap1, 10);

        // Second snapshot (should be identical pointer)
        let snap2 = manager.snapshot().await;
        assert!(Arc::ptr_eq(&snap1, &snap2));

        // Setup the lock to test mutation and publish
        let lock = manager.resolve_rwlock(|| async { Arc::new(10) }).await;

        // After write (CoW happens)
        {
            let mut guard = lock.write().await;
            *guard = 20;
            guard.commit(); // Ensure change is pushed back to StateManager
        }

        // After commit, snapshot changes
        let snap4 = manager.snapshot().await;
        assert_eq!(*snap4, 20);
        assert!(!Arc::ptr_eq(&snap1, &snap4));
    }

    #[tokio::test]
    async fn test_tracked_rwlock_notification() {
        let notify = Arc::new(Notify::new());
        let (tx, _rx) = watch::channel(Arc::new(0));
        let lock = TrackedRwLock {
            inner: TokioRwLock::new(Arc::new(0)),
            notify: notify.clone(),
            watch_tx: tx,
        };

        // Read doesn't notify
        {
            let _guard = lock.read().await;
            let wait = notify.notified();
            drop(_guard);
            assert!(!tokio::select! {
                _ = wait => true,
                _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => false,
            });
        }

        // Write WITHOUT mutation does NOT notify (spurious wakeup prevention)
        {
            let _guard = lock.write().await;
            let wait = notify.notified();
            drop(_guard);
            assert!(
                tokio::select! {
                    _ = wait => false,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => true,
                },
                "Write lock without mutation should NOT notify"
            );
        }

        // Write WITH mutation DOES notify
        {
            let mut guard = lock.write().await;
            *guard = 42; // Triggers DerefMut -> sets is_dirty
            let wait = notify.notified();
            drop(guard);
            assert!(
                tokio::select! {
                    _ = wait => true,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => false,
                },
                "Write lock with mutation should notify"
            );
        }
    }

    #[tokio::test]
    async fn test_tracked_rwlock_commit_and_publish() {
        let notify = Arc::new(Notify::new());
        let (tx, mut rx) = watch::channel(Arc::new(10));
        let lock = TrackedRwLock {
            inner: TokioRwLock::new(Arc::new(10)),
            notify: notify.clone(),
            watch_tx: tx,
        };

        // Test commit
        {
            let mut guard = lock.write().await;
            *guard = 20;
            guard.commit();
            assert_eq!(**rx.borrow_and_update(), 20);
        }

        // Test publish (zero-copy replacement)
        {
            let mut guard = lock.write().await;
            guard.publish(Arc::new(30));
        }
        assert_eq!(**rx.borrow_and_update(), 30);
    }

    #[tokio::test]
    async fn test_tracked_mutex_notification() {
        let notify = Arc::new(Notify::new());
        let (tx, _rx) = watch::channel(Arc::new(0));
        let rw = Arc::new(TrackedRwLock {
            inner: TokioRwLock::new(Arc::new(0)),
            notify: notify.clone(),
            watch_tx: tx,
        });
        let lock = TrackedMutex { inner: rw };

        // Lock WITHOUT mutation does NOT notify (spurious wakeup prevention)
        {
            let _guard = lock.lock().await;
            let wait = notify.notified();
            drop(_guard);
            assert!(
                tokio::select! {
                    _ = wait => false,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => true,
                },
                "Mutex lock without mutation should NOT notify"
            );
        }

        // Lock WITH mutation DOES notify
        {
            let mut guard = lock.lock().await;
            *guard = 99; // Triggers DerefMut -> sets is_dirty
            let wait = notify.notified();
            drop(guard);
            assert!(
                tokio::select! {
                    _ = wait => true,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => false,
                },
                "Mutex lock with mutation should notify"
            );
        }
    }
}
