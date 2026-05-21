use dashmap::DashMap;
use std::any::{Any, TypeId};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use tokio::sync::Notify as TokioNotify;

use crate::ProviderError;
use crate::core::context::api::current_provider_scope;
use crate::core::managed_state::{StateManager, TrackedMutex, TrackedRwLock};

const ROOT_PROVIDER_SCOPE_RAW_ID: u64 = 0;
static NEXT_PROVIDER_SCOPE_ID: AtomicU64 = AtomicU64::new(1);
static ROOT_PROVIDER_SCOPE: OnceLock<Arc<ProviderScope>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ProviderScopeId(u64);

impl ProviderScopeId {
    pub(crate) const fn root() -> Self {
        Self(ROOT_PROVIDER_SCOPE_RAW_ID)
    }

    #[cfg(test)]
    pub(crate) const fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ProviderSlotId {
    pub(crate) scope_id: ProviderScopeId,
    pub(crate) type_id: TypeId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProviderBindingKind {
    InheritedRoot,
    #[cfg(test)]
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
pub(crate) enum ProviderChangeKind {
    Value,
    Binding,
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
    pub(crate) fn root() -> Arc<Self> {
        ROOT_PROVIDER_SCOPE
            .get_or_init(|| Arc::new(Self::new(ProviderScopeId::root())))
            .clone()
    }

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
                scope_id: ProviderScopeId::root(),
                type_id,
            },
            kind: ProviderBindingKind::InheritedRoot,
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

    #[cfg(test)]
    pub(crate) fn create_empty_local_slot<T>(&self) -> ProviderBindingSnapshot
    where
        T: 'static + Send + Sync + Clone,
    {
        let type_id = TypeId::of::<T>();
        self.local_slots.entry(type_id).or_insert_with(|| {
            let slot: Arc<dyn Any + Send + Sync> = Arc::new(StateManager::<T>::new());
            slot
        });
        self.mutate_binding(type_id, ProviderBindingKind::Local)
    }

    #[cfg(test)]
    pub(crate) async fn fork_inherited_root_slot<T, F, Fut, E>(
        &self,
        root_manager: &StateManager<T>,
        init: F,
    ) -> Result<ProviderBindingSnapshot, E>
    where
        T: 'static + Send + Sync + Clone,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Arc<T>, E>> + Send,
    {
        let snapshot = root_manager.resolve_snapshot_result(init).await?;
        Ok(self.install_local_slot(snapshot, ProviderBindingKind::Local))
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

    fn local_slot_for_binding<T>(
        &self,
        binding: ProviderBindingSnapshot,
    ) -> Option<Arc<StateManager<T>>>
    where
        T: 'static + Send + Sync + Clone,
    {
        if binding.slot_id.scope_id != self.id {
            return None;
        }

        self.local_slot::<T>()
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

fn current_local_provider_manager<T>() -> Option<Arc<StateManager<T>>>
where
    T: 'static + Send + Sync + Clone,
{
    let scope = current_provider_scope();
    let binding = scope.effective_binding(TypeId::of::<T>());
    scope.local_slot_for_binding::<T>(binding)
}

pub async fn resolve_provider_snapshot<T, F, Fut, E>(
    root_manager: &'static StateManager<T>,
    init: F,
) -> Result<Arc<T>, E>
where
    T: 'static + Send + Sync + Clone,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Arc<T>, E>> + Send,
{
    if let Some(local_manager) = current_local_provider_manager::<T>() {
        local_manager.resolve_snapshot_result(init).await
    } else {
        root_manager.resolve_snapshot_result(init).await
    }
}

pub async fn resolve_provider_rwlock<T, F, Fut, E>(
    root_manager: &'static StateManager<T>,
    init: F,
) -> Result<Arc<TrackedRwLock<T>>, E>
where
    T: 'static + Send + Sync + Clone,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Arc<T>, E>> + Send,
{
    if let Some(local_manager) = current_local_provider_manager::<T>() {
        local_manager.resolve_rwlock_result(init).await
    } else {
        root_manager.resolve_rwlock_result(init).await
    }
}

pub async fn resolve_provider_mutex<T, F, Fut, E>(
    root_manager: &'static StateManager<T>,
    init: F,
) -> Result<Arc<TrackedMutex<T>>, E>
where
    T: 'static + Send + Sync + Clone,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Arc<T>, E>> + Send,
{
    if let Some(local_manager) = current_local_provider_manager::<T>() {
        local_manager.resolve_mutex_result(init).await
    } else {
        root_manager.resolve_mutex_result(init).await
    }
}

pub async fn resolve_provider_managed<T, F, Fut>(
    root_manager: &'static StateManager<T>,
    init: F,
) -> Result<Arc<T>, ProviderError>
where
    T: 'static + Send + Sync + Clone,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Arc<T>, ProviderError>> + Send,
{
    if let Some(local_manager) = current_local_provider_manager::<T>() {
        local_manager.resolve_snapshot_result(init).await
    } else {
        root_manager.resolve_managed_result(init).await
    }
}

async fn provider_change<T>(root_manager: &'static StateManager<T>) -> ProviderChangeKind
where
    T: 'static + Send + Sync + Clone,
{
    let scope = current_provider_scope();
    let type_id = TypeId::of::<T>();
    let binding = scope.effective_binding(type_id);

    if let Some(local_manager) = scope.local_slot_for_binding::<T>(binding) {
        tokio::select! {
            _ = local_manager.changed() => ProviderChangeKind::Value,
            _ = scope.binding_changed_from::<T>(binding) => ProviderChangeKind::Binding,
        }
    } else {
        tokio::select! {
            _ = root_manager.changed() => ProviderChangeKind::Value,
            _ = scope.binding_changed_from::<T>(binding) => ProviderChangeKind::Binding,
        }
    }
}

pub async fn provider_changed<T>(root_manager: &'static StateManager<T>)
where
    T: 'static + Send + Sync + Clone,
{
    let _ = provider_change(root_manager).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct ProviderA;

    #[derive(Clone)]
    struct ProviderB;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct ProviderValue(usize);

    fn root_manager(value: usize) -> &'static StateManager<ProviderValue> {
        Box::leak(Box::new(StateManager::with_value(ProviderValue(value))))
    }

    async fn resolve_snapshot(
        root_manager: &'static StateManager<ProviderValue>,
    ) -> Arc<ProviderValue> {
        resolve_provider_snapshot(root_manager, || async {
            Ok::<Arc<ProviderValue>, ()>(Arc::new(ProviderValue(99)))
        })
        .await
        .expect("provider snapshot should resolve")
    }

    fn spawn_change_waiter(
        root_manager: &'static StateManager<ProviderValue>,
        resources: Arc<crate::core::context::DaemonResources>,
    ) -> tokio::task::JoinHandle<ProviderChangeKind> {
        tokio::spawn(async move {
            crate::core::context::__run_daemon_resources_scope(resources, || async {
                provider_change(root_manager).await
            })
            .await
        })
    }

    fn spawn_binding_waiter(
        scope: Arc<ProviderScope>,
        observed: ProviderBindingSnapshot,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            scope.binding_changed_from::<ProviderValue>(observed).await;
        })
    }

    async fn await_change(
        waiter: tokio::task::JoinHandle<ProviderChangeKind>,
    ) -> ProviderChangeKind {
        tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
            .await
            .expect("provider change waiter should not time out")
            .expect("provider change waiter should join")
    }

    #[test]
    fn root_scope_identity_is_stable() {
        let first = ProviderScope::root();
        let second = ProviderScope::root();

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.id(), ProviderScopeId::root());
        assert_eq!(first.id().raw(), ROOT_PROVIDER_SCOPE_RAW_ID);
    }

    #[test]
    fn daemon_scopes_receive_distinct_ids() {
        let first = ProviderScope::new_daemon_scope();
        let second = ProviderScope::new_daemon_scope();

        assert_ne!(first.id(), ProviderScopeId::root());
        assert_ne!(second.id(), ProviderScopeId::root());
        assert_ne!(first.id(), second.id());
    }

    #[test]
    fn daemon_scope_defaults_to_inherited_root_binding() {
        let scope = ProviderScope::new_daemon_scope();
        let type_id = TypeId::of::<ProviderA>();

        let binding = scope.effective_binding(type_id);

        assert_eq!(binding.kind, ProviderBindingKind::InheritedRoot);
        assert_eq!(binding.slot_id.scope_id, ProviderScopeId::root());
        assert_eq!(binding.slot_id.type_id, type_id);
        assert_eq!(binding.epoch, 0);
    }

    #[test]
    fn local_binding_shadows_inherited_root_binding() {
        let scope = ProviderScope::new_daemon_scope();
        let type_id = TypeId::of::<ProviderA>();

        let binding = scope.create_empty_local_slot::<ProviderA>();

        assert_eq!(binding.kind, ProviderBindingKind::Local);
        assert_eq!(binding.slot_id.scope_id, scope.id());
        assert_eq!(binding.epoch, 1);
        assert_eq!(scope.effective_binding(type_id), binding);
    }

    #[test]
    fn override_binding_shadows_local_binding_with_new_epoch() {
        let scope = ProviderScope::new_daemon_scope();

        let local = scope.create_empty_local_slot::<ProviderA>();
        let override_binding = scope.override_local_slot(Arc::new(ProviderA));

        assert_eq!(local.kind, ProviderBindingKind::Local);
        assert_eq!(override_binding.kind, ProviderBindingKind::Override);
        assert_eq!(override_binding.slot_id.scope_id, scope.id());
        assert_eq!(override_binding.epoch, local.epoch + 1);
    }

    #[test]
    fn binding_epoch_changes_only_for_mutated_binding() {
        let scope = ProviderScope::new_daemon_scope();
        let provider_a = TypeId::of::<ProviderA>();
        let provider_b = TypeId::of::<ProviderB>();

        let first = scope.create_empty_local_slot::<ProviderA>();
        let unchanged = scope.effective_binding(provider_a);
        let unrelated = scope.effective_binding(provider_b);

        assert_eq!(unchanged.epoch, first.epoch);
        assert_eq!(unrelated.epoch, 0);
        assert_eq!(unrelated.kind, ProviderBindingKind::InheritedRoot);
    }

    #[tokio::test]
    async fn daemon_local_override_shadows_root_without_affecting_other_scopes() {
        let root_manager = root_manager(1);
        let first_resources = crate::core::context::DaemonResources::new();
        let second_resources = crate::core::context::DaemonResources::new();

        first_resources
            .provider_scope
            .override_local_slot(Arc::new(ProviderValue(2)));

        let first = crate::core::context::__run_daemon_resources_scope(first_resources, || async {
            resolve_snapshot(root_manager).await
        })
        .await;
        let second =
            crate::core::context::__run_daemon_resources_scope(second_resources, || async {
                resolve_snapshot(root_manager).await
            })
            .await;
        let external = resolve_snapshot(root_manager).await;

        assert_eq!(first.0, 2);
        assert_eq!(second.0, 1);
        assert_eq!(external.0, 1);
    }

    #[tokio::test]
    async fn daemon_local_value_mutation_does_not_change_root_slot() {
        let root_manager = root_manager(1);
        let resources = crate::core::context::DaemonResources::new();
        resources
            .provider_scope
            .override_local_slot(Arc::new(ProviderValue(2)));

        let local_lock =
            crate::core::context::__run_daemon_resources_scope(resources.clone(), || async {
                resolve_provider_rwlock(root_manager, || async {
                    Ok::<Arc<ProviderValue>, ()>(Arc::new(ProviderValue(99)))
                })
                .await
                .expect("local rwlock should resolve")
            })
            .await;
        {
            let mut guard = local_lock.write().await;
            guard.0 = 3;
        }

        let local = crate::core::context::__run_daemon_resources_scope(resources, || async {
            resolve_snapshot(root_manager).await
        })
        .await;
        let root = resolve_snapshot(root_manager).await;

        assert_eq!(local.0, 3);
        assert_eq!(root.0, 1);
    }

    #[tokio::test]
    async fn root_value_mutation_reaches_inherited_scope_but_not_forked_scope() {
        let root_manager = root_manager(1);
        let forked_resources = crate::core::context::DaemonResources::new();
        let inherited_resources = crate::core::context::DaemonResources::new();

        forked_resources
            .provider_scope
            .fork_inherited_root_slot(root_manager, || async {
                Ok::<Arc<ProviderValue>, ()>(Arc::new(ProviderValue(99)))
            })
            .await
            .expect("root slot should fork");

        let root_lock = resolve_provider_rwlock(root_manager, || async {
            Ok::<Arc<ProviderValue>, ()>(Arc::new(ProviderValue(99)))
        })
        .await
        .expect("root rwlock should resolve");
        {
            let mut guard = root_lock.write().await;
            guard.0 = 2;
        }

        let forked =
            crate::core::context::__run_daemon_resources_scope(forked_resources, || async {
                resolve_snapshot(root_manager).await
            })
            .await;
        let inherited =
            crate::core::context::__run_daemon_resources_scope(inherited_resources, || async {
                resolve_snapshot(root_manager).await
            })
            .await;

        assert_eq!(forked.0, 1);
        assert_eq!(inherited.0, 2);
    }

    #[tokio::test]
    async fn provider_changed_waits_on_daemon_local_slot_when_bound() {
        let root_manager = root_manager(1);
        let resources = crate::core::context::DaemonResources::new();
        resources
            .provider_scope
            .override_local_slot(Arc::new(ProviderValue(2)));

        let local_lock =
            crate::core::context::__run_daemon_resources_scope(resources.clone(), || async {
                resolve_provider_rwlock(root_manager, || async {
                    Ok::<Arc<ProviderValue>, ()>(Arc::new(ProviderValue(99)))
                })
                .await
                .expect("local rwlock should resolve")
            })
            .await;
        let changed = tokio::spawn({
            let resources = resources.clone();
            async move {
                crate::core::context::__run_daemon_resources_scope(resources, || async {
                    provider_change(root_manager).await
                })
                .await
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        {
            let mut guard = local_lock.write().await;
            guard.0 = 3;
        }

        let kind = tokio::time::timeout(std::time::Duration::from_secs(5), changed)
            .await
            .expect("local provider change should notify")
            .expect("change watcher task should join");
        assert_eq!(kind, ProviderChangeKind::Value);
    }

    #[tokio::test]
    async fn provider_changed_wakes_on_binding_mutation() {
        let root_manager = root_manager(1);
        let resources = crate::core::context::DaemonResources::new();
        let changed = tokio::spawn({
            let resources = resources.clone();
            async move {
                crate::core::context::__run_daemon_resources_scope(resources, || async {
                    provider_change(root_manager).await
                })
                .await
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        resources
            .provider_scope
            .override_local_slot(Arc::new(ProviderValue(2)));

        let kind = tokio::time::timeout(std::time::Duration::from_secs(5), changed)
            .await
            .expect("binding mutation should notify")
            .expect("binding watcher task should join");
        assert_eq!(kind, ProviderChangeKind::Binding);
    }

    #[tokio::test]
    async fn binding_waiter_returns_when_binding_already_changed() {
        let scope = ProviderScope::new_daemon_scope();
        let observed = scope.effective_binding(TypeId::of::<ProviderValue>());

        scope.override_local_slot(Arc::new(ProviderValue(2)));

        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            scope.binding_changed_from::<ProviderValue>(observed),
        )
        .await
        .expect("binding waiter should observe the changed epoch");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_binding_mutations_advance_epoch_without_lost_updates() {
        let scope = ProviderScope::new_daemon_scope();
        let mut tasks = Vec::new();

        for value in 1..=32 {
            let scope = scope.clone();
            tasks.push(tokio::spawn(async move {
                scope.override_local_slot(Arc::new(ProviderValue(value)))
            }));
        }

        let mut epochs = Vec::new();
        for task in tasks {
            epochs.push(
                task.await
                    .expect("concurrent binding mutation task should join")
                    .epoch,
            );
        }
        epochs.sort_unstable();

        let binding = scope.effective_binding(TypeId::of::<ProviderValue>());
        assert_eq!(binding.kind, ProviderBindingKind::Override);
        assert_eq!(binding.epoch, 32);
        assert_eq!(epochs, (1..=32).collect::<Vec<_>>());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn binding_mutation_wakes_all_concurrent_binding_waiters() {
        let scope = ProviderScope::new_daemon_scope();
        let observed = scope.effective_binding(TypeId::of::<ProviderValue>());
        let mut waiters = Vec::new();

        for _ in 0..16 {
            waiters.push(spawn_binding_waiter(scope.clone(), observed));
        }

        scope.override_local_slot(Arc::new(ProviderValue(2)));

        for waiter in waiters {
            tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
                .await
                .expect("binding waiter should not time out")
                .expect("binding waiter should join");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn local_value_mutation_wakes_only_local_slot_waiters() {
        let root_manager = root_manager(1);
        let local_resources = crate::core::context::DaemonResources::new();
        let inherited_resources = crate::core::context::DaemonResources::new();
        local_resources
            .provider_scope
            .override_local_slot(Arc::new(ProviderValue(2)));

        let local_lock =
            crate::core::context::__run_daemon_resources_scope(local_resources.clone(), || async {
                resolve_provider_rwlock(root_manager, || async {
                    Ok::<Arc<ProviderValue>, ()>(Arc::new(ProviderValue(99)))
                })
                .await
                .expect("local rwlock should resolve")
            })
            .await;
        let local_waiter = spawn_change_waiter(root_manager, local_resources);
        let mut inherited_waiter = spawn_change_waiter(root_manager, inherited_resources);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        {
            let mut guard = local_lock.write().await;
            guard.0 = 3;
        }

        assert_eq!(await_change(local_waiter).await, ProviderChangeKind::Value);
        tokio::select! {
            result = &mut inherited_waiter => {
                panic!("inherited waiter woke from daemon-local value mutation: {result:?}");
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(150)) => {}
        }
        inherited_waiter.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_value_and_binding_changes_wake_waiters_and_preserve_final_boundary() {
        let root_manager = root_manager(1);
        let resources = crate::core::context::DaemonResources::new();
        let root_lock = resolve_provider_rwlock(root_manager, || async {
            Ok::<Arc<ProviderValue>, ()>(Arc::new(ProviderValue(99)))
        })
        .await
        .expect("root rwlock should resolve");
        let mut waiters = Vec::new();

        for _ in 0..16 {
            waiters.push(spawn_change_waiter(root_manager, resources.clone()));
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let value_mutation = tokio::spawn(async move {
            let mut guard = root_lock.write().await;
            guard.0 = 2;
        });
        resources
            .provider_scope
            .override_local_slot(Arc::new(ProviderValue(3)));
        value_mutation
            .await
            .expect("root value mutation task should join");

        for waiter in waiters {
            let kind = await_change(waiter).await;
            assert!(
                matches!(
                    kind,
                    ProviderChangeKind::Value | ProviderChangeKind::Binding
                ),
                "unexpected provider change kind: {kind:?}"
            );
        }

        let scoped = crate::core::context::__run_daemon_resources_scope(resources, || async {
            resolve_snapshot(root_manager).await
        })
        .await;
        let root = resolve_snapshot(root_manager).await;

        assert_eq!(scoped.0, 3);
        assert_eq!(root.0, 2);
    }
}
