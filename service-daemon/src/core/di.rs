use crate::core::managed_state::{Mutex, RwLock};
use crate::{ProviderError, ProviderInitError};
use futures::future::{BoxFuture, pending};
use futures::stream::{FuturesUnordered, StreamExt};
use std::any::TypeId;
use std::future::Future;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderDependencyChangeReason {
    Value,
    Binding,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderDependencyChange {
    pub type_id: TypeId,
    pub reason: ProviderDependencyChangeReason,
}

impl ProviderDependencyChange {
    pub(crate) const fn new(type_id: TypeId, reason: ProviderDependencyChangeReason) -> Self {
        Self { type_id, reason }
    }
}

pub struct ProviderDependencyWatch {
    changed: BoxFuture<'static, ProviderDependencyChange>,
}

impl ProviderDependencyWatch {
    pub(crate) fn new(
        changed: impl Future<Output = ProviderDependencyChange> + Send + 'static,
    ) -> Self {
        Self {
            changed: Box::pin(changed),
        }
    }

    pub async fn changed(self) -> ProviderDependencyChange {
        self.changed.await
    }
}

#[derive(Default)]
pub struct ProviderDependencyWatchSet {
    watches: Vec<ProviderDependencyWatch>,
}

impl ProviderDependencyWatchSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, watch: ProviderDependencyWatch) {
        self.watches.push(watch);
    }

    pub fn is_empty(&self) -> bool {
        self.watches.is_empty()
    }

    pub async fn changed(self) -> ProviderDependencyChange {
        if self.watches.is_empty() {
            return pending::<ProviderDependencyChange>().await;
        }

        let mut watches = FuturesUnordered::new();
        for watch in self.watches {
            watches.push(watch.changed);
        }

        match watches.next().await {
            Some(change) => change,
            None => pending::<ProviderDependencyChange>().await,
        }
    }
}

/// A trait for types that can be resolved by the DI system as read-only snapshots.
///
/// This trait is typically implemented by the `#[provider]` macro. All
/// `#[provider]` forms currently auto-generate `Provided`, `ManagedProvided`,
/// and `WatchableProvided` together.
///
/// Generated providers resolve through the current daemon's effective provider
/// scope when called from a service, trigger, watcher, or daemon startup path.
/// Calls made outside a daemon context fall back to the root provider scope.
///
/// If you see a compile error about this trait not being implemented, it means
/// you forgot to add `#[provider]` for that type.
#[diagnostic::on_unimplemented(
    message = "Missing Provider: The type `{Self}` cannot be injected.",
    label = "this requires `{Self}: Provided`",
    note = "Add `#[provider]` to a function returning `{Self}`, or use `#[provider]` on the struct definition."
)]
pub trait Provided: 'static + Send + Sync + Clone + Sized {
    /// Resolves a read-only snapshot from the current effective provider slot.
    ///
    /// In service/trigger/daemon contexts this uses the daemon's provider scope;
    /// outside those contexts it uses the root provider scope fallback.
    /// If the provider has been promoted to managed state, this returns the
    /// latest published snapshot for that slot.
    fn resolve()
    -> impl std::future::Future<Output = std::result::Result<Arc<Self>, ProviderInitError>> + Send;
}

#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "Registered Provider required: `{Self}` must be declared with `#[provider]`.",
    label = "this injection requires a `#[provider]` registration for `{Self}`",
    note = "Add `#[provider]` to a function returning `{Self}`, or use `#[provider]` on the struct definition."
)]
pub trait ProviderDefinition: ManagedProvided {
    fn ready_snapshot() -> std::result::Result<Arc<Self>, ProviderInitError>;

    fn ready_rwlock() -> std::result::Result<Arc<RwLock<Self>>, ProviderInitError>;

    fn ready_mutex() -> std::result::Result<Arc<Mutex<Self>>, ProviderInitError>;
}

/// Marker trait for a shared provider contract type.
///
/// `#[provider_contract]` implements this trait on the shared output type. A
/// separate crate can then register one or more `#[provider_impl]` candidates
/// for that output without implementing DI traits on the foreign type.
#[diagnostic::on_unimplemented(
    message = "Provider Contract required: `{Self}` must be marked with `#[provider_contract]`.",
    label = "this provider implementation returns `{Self}`, which is not a provider contract",
    note = "Add `#[provider_contract]` to the shared output type, then register implementations with `#[provider_impl]`."
)]
pub trait ProviderContract: 'static + Send + Sync + Clone {}

/// A trait for provider types that support managed mutable state.
///
/// This capability is required for `Arc<RwLock<T>>` and `Arc<Mutex<T>>`
/// injection. The `#[provider]` macro auto-generates this impl by delegating to
/// the effective provider slot's `StateManager`.
///
/// In a daemon context, managed state belongs to that daemon's effective slot:
/// usually the inherited root slot, or a daemon-local slot after simulation
/// override/internal fork. Outside a daemon context, helper calls use the root
/// slot fallback.
///
/// Provider injection is defined by `#[provider]`; direct hand-written provider
/// capability impls do not register dependency metadata and are not supported
/// as DI entry points.
#[diagnostic::on_unimplemented(
    message = "Managed Provider required: `{Self}` cannot be injected as `Arc<RwLock<_>>` or `Arc<Mutex<_>>`.",
    label = "this injection requires `{Self}: ManagedProvided`",
    note = "Add `#[provider]` to let the macro generate managed-state support for `{Self}`."
)]
pub trait ManagedProvided: Provided {
    /// Resolves a live tracked `RwLock` for this type.
    fn resolve_rwlock()
    -> impl std::future::Future<Output = std::result::Result<Arc<RwLock<Self>>, ProviderInitError>>
    + Send;

    /// Resolves a live tracked `Mutex` for this type.
    fn resolve_mutex()
    -> impl std::future::Future<Output = std::result::Result<Arc<Mutex<Self>>, ProviderInitError>> + Send;

    /// Resolves the raw initialization result for this provider.
    fn resolve_managed()
    -> impl std::future::Future<Output = std::result::Result<Arc<Self>, ProviderError>> + Send;
}

/// A trait for managed provider types that also support dependency watching.
///
/// This capability is required for `Watch(T)` triggers and for service/trigger
/// dependency reloads. The default `#[provider]` implementation captures the
/// current effective provider slot's value epoch and binding snapshot when the
/// watch handle is created. A watch therefore wakes when a managed snapshot is
/// published, or when the daemon switches that provider type to a local
/// override/fork.
///
/// Current pre-release behavior: `#[provider]` owns this implementation. Direct
/// hand-written provider capability impls are not supported as DI entry points.
#[diagnostic::on_unimplemented(
    message = "Watchable Provider required: `{Self}` cannot be used with `Watch(...)` triggers.",
    label = "this trigger requires `{Self}: WatchableProvided`",
    note = "Add `#[provider]` to let the macro generate watch support for `{Self}`."
)]
pub trait WatchableProvided: ManagedProvided {
    /// Captures the current dependency baseline and returns a watch handle for later awaiting.
    fn watch_dependency() -> ProviderDependencyWatch;
}
