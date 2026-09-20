use dashmap::DashMap;
use std::any::{Any, TypeId};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Notify as TokioNotify;

use crate::ProviderError;
use crate::core::context::api::current_provider_scope;
use crate::core::di::{
    ProviderDependencyChange, ProviderDependencyChangeReason, ProviderDependencyWatch,
};
use crate::core::managed_state::{StateManager, TrackedMutex, TrackedRwLock};
use crate::models::ProviderInitError;

static NEXT_PROVIDER_SCOPE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ProviderScopeId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ProviderSlotId {
    pub(crate) scope_id: ProviderScopeId,
    pub(crate) type_id: TypeId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProviderBindingKind {
    Local,
    #[cfg(any(test, feature = "simulation"))]
    Override,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProviderBindingSnapshot {
    pub(crate) slot_id: ProviderSlotId,
    pub(crate) kind: ProviderBindingKind,
    pub(crate) epoch: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProviderBinding {
    kind: ProviderBindingKind,
    epoch: u64,
}

pub(crate) struct ProviderScope {
    id: ProviderScopeId,
    local_bindings: DashMap<TypeId, ProviderBinding>,
    local_slots: DashMap<TypeId, Arc<dyn Any + Send + Sync>>,
    binding_changes: DashMap<TypeId, Arc<TokioNotify>>,
}

impl ProviderScope {
    pub(crate) fn new_daemon_scope() -> Arc<Self> {
        let id = ProviderScopeId(NEXT_PROVIDER_SCOPE_ID.fetch_add(1, Ordering::Relaxed));
        Arc::new(Self::new(id))
    }

    fn new(id: ProviderScopeId) -> Self {
        Self {
            id,
            local_bindings: DashMap::new(),
            local_slots: DashMap::new(),
            binding_changes: DashMap::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn id(&self) -> ProviderScopeId {
        self.id
    }

    pub(crate) fn effective_binding(&self, type_id: TypeId) -> ProviderBindingSnapshot {
        if let Some(binding) = self.local_bindings.get(&type_id) {
            return ProviderBindingSnapshot {
                slot_id: ProviderSlotId {
                    scope_id: self.id,
                    type_id,
                },
                kind: binding.kind,
                epoch: binding.epoch,
            };
        }

        ProviderBindingSnapshot {
            slot_id: ProviderSlotId {
                scope_id: self.id,
                type_id,
            },
            kind: ProviderBindingKind::Local,
            epoch: 0,
        }
    }

    fn binding_notify(&self, type_id: TypeId) -> Arc<TokioNotify> {
        self.binding_changes
            .entry(type_id)
            .or_insert_with(|| Arc::new(TokioNotify::new()))
            .clone()
    }

    async fn binding_changed_from<T>(&self, observed: ProviderBindingSnapshot)
    where
        T: 'static + Send + Sync + Clone,
    {
        let type_id = TypeId::of::<T>();
        loop {
            let binding_notify = self.binding_notify(type_id);
            let changed = binding_notify.notified();
            tokio::pin!(changed);
            let _ = changed.as_mut().enable();

            if self.effective_binding(type_id) != observed {
                return;
            }

            changed.await;
        }
    }

    #[cfg(any(test, feature = "simulation"))]
    fn notify_binding_changed(&self, type_id: TypeId) {
        if let Some(notify) = self.binding_changes.get(&type_id) {
            notify.notify_waiters();
        }
    }

    fn ensure_local_slot<T>(&self) -> Arc<StateManager<T>>
    where
        T: 'static + Send + Sync + Clone,
    {
        let slot = self
            .local_slots
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Arc::new(StateManager::<T>::new()))
            .clone();
        match slot.downcast::<StateManager<T>>() {
            Ok(manager) => manager,
            Err(_) => unreachable!("provider slot type must match its TypeId"),
        }
    }

    #[cfg(any(test, feature = "simulation"))]
    pub(crate) fn override_local_slot<T>(&self, value: Arc<T>) -> ProviderBindingSnapshot
    where
        T: 'static + Send + Sync + Clone,
    {
        self.install_local_slot(value, ProviderBindingKind::Override)
    }

    fn local_slot<T>(&self) -> Option<Arc<StateManager<T>>>
    where
        T: 'static + Send + Sync + Clone,
    {
        self.local_slots
            .get(&TypeId::of::<T>())
            .and_then(|entry| entry.value().clone().downcast::<StateManager<T>>().ok())
    }

    #[cfg(any(test, feature = "simulation"))]
    fn install_local_slot<T>(
        &self,
        value: Arc<T>,
        kind: ProviderBindingKind,
    ) -> ProviderBindingSnapshot
    where
        T: 'static + Send + Sync + Clone,
    {
        let type_id = TypeId::of::<T>();
        let slot: Arc<dyn Any + Send + Sync> = Arc::new(StateManager::with_arc(value));
        self.local_slots.insert(type_id, slot);
        self.mutate_binding(type_id, kind)
    }

    #[cfg(any(test, feature = "simulation"))]
    fn mutate_binding(
        &self,
        type_id: TypeId,
        kind: ProviderBindingKind,
    ) -> ProviderBindingSnapshot {
        let mut binding = self
            .local_bindings
            .entry(type_id)
            .or_insert(ProviderBinding { kind, epoch: 0 });
        binding.kind = kind;
        binding.epoch = binding.epoch.saturating_add(1);
        let epoch = binding.epoch;
        drop(binding);
        self.notify_binding_changed(type_id);

        ProviderBindingSnapshot {
            slot_id: ProviderSlotId {
                scope_id: self.id,
                type_id,
            },
            kind,
            epoch,
        }
    }
}

fn require_provider_scope(provider: &str) -> Result<Arc<ProviderScope>, ProviderInitError> {
    current_provider_scope().ok_or_else(|| ProviderInitError::Fatal {
        provider: provider.to_owned(),
        message: "provider resolution requires a daemon context".to_owned(),
    })
}

fn current_provider_manager<T>() -> Result<Arc<StateManager<T>>, ProviderInitError>
where
    T: 'static + Send + Sync + Clone,
{
    Ok(require_provider_scope(std::any::type_name::<T>())?.ensure_local_slot::<T>())
}

pub fn missing_prepared_provider_error(
    provider: &'static str,
    dependency: &'static str,
) -> ProviderInitError {
    ProviderInitError::Fatal {
        provider: provider.to_owned(),
        message: format!(
            "provider dependency `{dependency}` was not prepared before constructing `{provider}`"
        ),
    }
}

pub fn require_provider_context(provider: &str) -> Result<(), ProviderInitError> {
    require_provider_scope(provider).map(|_| ())
}

pub fn ready_provider_snapshot<T>() -> Result<Option<Arc<T>>, ProviderInitError>
where
    T: 'static + Send + Sync + Clone,
{
    let scope = require_provider_scope(std::any::type_name::<T>())?;
    Ok(scope
        .local_slot::<T>()
        .and_then(|manager| manager.snapshot_ready()))
}

pub async fn resolve_provider_snapshot<T, F, Fut>(init: F) -> Result<Arc<T>, ProviderInitError>
where
    T: 'static + Send + Sync + Clone,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Arc<T>, ProviderInitError>> + Send,
{
    current_provider_manager::<T>()?
        .resolve_snapshot_result(init)
        .await
}

pub fn ready_provider_rwlock<T>() -> Result<Option<Arc<TrackedRwLock<T>>>, ProviderInitError>
where
    T: 'static + Send + Sync + Clone,
{
    let scope = require_provider_scope(std::any::type_name::<T>())?;
    Ok(scope
        .local_slot::<T>()
        .and_then(|manager| manager.rwlock_ready()))
}

pub async fn resolve_provider_rwlock<T, F, Fut>(
    init: F,
) -> Result<Arc<TrackedRwLock<T>>, ProviderInitError>
where
    T: 'static + Send + Sync + Clone,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Arc<T>, ProviderInitError>> + Send,
{
    current_provider_manager::<T>()?
        .resolve_rwlock_result(init)
        .await
}

pub fn ready_provider_mutex<T>() -> Result<Option<Arc<TrackedMutex<T>>>, ProviderInitError>
where
    T: 'static + Send + Sync + Clone,
{
    let scope = require_provider_scope(std::any::type_name::<T>())?;
    Ok(scope
        .local_slot::<T>()
        .and_then(|manager| manager.mutex_ready()))
}

pub async fn resolve_provider_mutex<T, F, Fut>(
    init: F,
) -> Result<Arc<TrackedMutex<T>>, ProviderInitError>
where
    T: 'static + Send + Sync + Clone,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Arc<T>, ProviderInitError>> + Send,
{
    current_provider_manager::<T>()?
        .resolve_mutex_result(init)
        .await
}

pub async fn resolve_provider_managed<T, F, Fut>(init: F) -> Result<Arc<T>, ProviderError>
where
    T: 'static + Send + Sync + Clone,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Arc<T>, ProviderError>> + Send,
{
    current_provider_manager::<T>()
        .map_err(|error| ProviderError::Fatal(error.to_string()))?
        .resolve_snapshot_result(init)
        .await
}

pub fn provider_dependency_watch<T>() -> ProviderDependencyWatch
where
    T: 'static + Send + Sync + Clone,
{
    let scope = require_provider_scope(std::any::type_name::<T>())
        .unwrap_or_else(|error| panic!("cannot create provider dependency watch: {error}"));
    let type_id = TypeId::of::<T>();
    let binding = scope.effective_binding(type_id);
    let manager = scope.ensure_local_slot::<T>();
    let observed_epoch = manager.value_epoch();
    ProviderDependencyWatch::new(async move {
        tokio::select! {
            _ = manager.changed_since(observed_epoch) => {
                ProviderDependencyChange::new(type_id, ProviderDependencyChangeReason::Value)
            }
            _ = scope.binding_changed_from::<T>(binding) => {
                ProviderDependencyChange::new(type_id, ProviderDependencyChangeReason::Binding)
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context::{__run_daemon_resources_scope, DaemonResources};
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    #[derive(Clone, Debug)]
    struct Value(usize);

    async fn resolve(value: usize) -> Arc<Value> {
        resolve_provider_snapshot(|| async { Ok(Arc::new(Value(value))) })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn scopes_isolate_values_and_preserve_identity() {
        let first = DaemonResources::new();
        let second = DaemonResources::new();
        assert_ne!(first.provider_scope.id(), second.provider_scope.id());
        let first_value = __run_daemon_resources_scope(first.clone(), || resolve(1)).await;
        let again = __run_daemon_resources_scope(first, || resolve(99)).await;
        let second_value = __run_daemon_resources_scope(second, || resolve(2)).await;
        assert!(Arc::ptr_eq(&first_value, &again));
        assert_eq!(second_value.0, 2);
        assert!(!Arc::ptr_eq(&first_value, &second_value));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_resolution_initializes_once_without_changing_binding() {
        let resources = DaemonResources::new();
        let before = resources
            .provider_scope
            .effective_binding(TypeId::of::<Value>());
        let count = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..32 {
            let resources = resources.clone();
            let count = count.clone();
            tasks.push(tokio::spawn(async move {
                __run_daemon_resources_scope(resources, || async {
                    resolve_provider_snapshot(|| async {
                        count.fetch_add(1, Ordering::SeqCst);
                        tokio::task::yield_now().await;
                        Ok(Arc::new(Value(1)))
                    })
                    .await
                    .unwrap()
                })
                .await
            }));
        }
        let mut values = Vec::new();
        for task in tasks {
            values.push(task.await.unwrap());
        }
        assert!(values.iter().all(|value| Arc::ptr_eq(&values[0], value)));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(
            before,
            resources
                .provider_scope
                .effective_binding(TypeId::of::<Value>())
        );
    }

    #[tokio::test]
    async fn watch_observes_changes_before_await_and_only_in_own_daemon() {
        let first = DaemonResources::new();
        let second = DaemonResources::new();
        __run_daemon_resources_scope(first.clone(), || resolve(1)).await;
        __run_daemon_resources_scope(second.clone(), || resolve(2)).await;
        let first_watch = __run_daemon_resources_scope(first.clone(), || async {
            provider_dependency_watch::<Value>()
        })
        .await;
        let second_watch =
            __run_daemon_resources_scope(second, || async { provider_dependency_watch::<Value>() })
                .await;
        let lock = __run_daemon_resources_scope(first, || async {
            resolve_provider_rwlock(|| async { Ok(Arc::new(Value(99))) })
                .await
                .unwrap()
        })
        .await;
        lock.write().await.0 = 3;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), first_watch.changed())
                .await
                .unwrap()
                .reason,
            ProviderDependencyChangeReason::Value
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(30), second_watch.changed())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn override_rebinds_watch_and_detaches_old_value() {
        let resources = DaemonResources::new();
        let old = __run_daemon_resources_scope(resources.clone(), || async {
            resolve_provider_rwlock(|| async { Ok(Arc::new(Value(1))) })
                .await
                .unwrap()
        })
        .await;
        let watch = __run_daemon_resources_scope(resources.clone(), || async {
            provider_dependency_watch::<Value>()
        })
        .await;
        resources
            .provider_scope
            .override_local_slot(Arc::new(Value(2)));
        assert_eq!(
            watch.changed().await.reason,
            ProviderDependencyChangeReason::Binding
        );
        let new_watch = __run_daemon_resources_scope(resources.clone(), || async {
            provider_dependency_watch::<Value>()
        })
        .await;
        old.write().await.0 = 99;
        assert!(
            tokio::time::timeout(Duration::from_millis(30), new_watch.changed())
                .await
                .is_err()
        );
        assert_eq!(
            __run_daemon_resources_scope(resources, || resolve(3))
                .await
                .0,
            2
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_overrides_advance_epoch_and_wake_all_watchers() {
        let resources = DaemonResources::new();
        let mut watches = Vec::new();
        for _ in 0..16 {
            watches.push(
                __run_daemon_resources_scope(resources.clone(), || async {
                    provider_dependency_watch::<Value>()
                })
                .await,
            );
        }
        let mut tasks = Vec::new();
        for value in 0..32 {
            let scope = resources.provider_scope.clone();
            tasks.push(tokio::spawn(async move {
                scope.override_local_slot(Arc::new(Value(value))).epoch
            }));
        }
        let mut epochs = Vec::new();
        for task in tasks {
            epochs.push(task.await.unwrap());
        }
        epochs.sort_unstable();
        assert_eq!(epochs, (1..=32).collect::<Vec<_>>());
        for watch in watches {
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(1), watch.changed())
                    .await
                    .unwrap()
                    .reason,
                ProviderDependencyChangeReason::Binding
            );
        }
    }

    #[tokio::test]
    async fn values_are_released_with_scope_and_external_holders() {
        let resources = DaemonResources::new();
        let value = __run_daemon_resources_scope(resources.clone(), || resolve(1)).await;
        let weak = Arc::downgrade(&value);
        drop(resources);
        assert!(weak.upgrade().is_some());
        drop(value);
        assert!(weak.upgrade().is_none());
    }
}
