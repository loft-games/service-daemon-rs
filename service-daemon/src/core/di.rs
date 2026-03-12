use crate::core::managed_state::{Mutex, RwLock};
use std::sync::Arc;

/// A trait for types that can be resolved by the DI system as read-only snapshots.
///
/// This trait is typically implemented by the `#[provider]` macro. All
/// `#[provider]` forms currently auto-generate `Provided`, `ManagedProvided`,
/// and `WatchableProvided` together.
///
/// If you see a compile error about this trait not being implemented, it means
/// you forgot to add `#[provider]` for that type or write a manual provider impl.
#[diagnostic::on_unimplemented(
    message = "Missing Provider: The type `{Self}` cannot be injected.",
    label = "this requires `{Self}: Provided`",
    note = "Add `#[provider]` to a function returning `{Self}`, or use `#[provider]` on the struct definition."
)]
pub trait Provided: 'static + Send + Sync + Clone + Sized {
    /// Resolves a read-only snapshot of this type.
    ///
    /// If the provider has been promoted to managed state, this returns the
    /// latest published snapshot. Otherwise, it returns the global immutable
    /// singleton value.
    fn resolve() -> impl std::future::Future<Output = Arc<Self>> + Send;
}

/// A trait for provider types that support managed mutable state.
///
/// This capability is required for `Arc<RwLock<T>>` and `Arc<Mutex<T>>`
/// injection. The `#[provider]` macro auto-generates this impl by delegating to
/// `StateManager`.
///
/// Current pre-release behavior: `#[provider]` does not try to defer to manual
/// impls. If you also hand-write `ManagedProvided` for the same type, Rust will
/// emit the normal duplicate-impl compile error.
#[diagnostic::on_unimplemented(
    message = "Managed Provider required: `{Self}` cannot be injected as `Arc<RwLock<_>>` or `Arc<Mutex<_>>`.",
    label = "this injection requires `{Self}: ManagedProvided`",
    note = "Add `#[provider]` to let the macro generate managed-state support, or implement `ManagedProvided` manually for `{Self}`."
)]
pub trait ManagedProvided: Provided {
    /// Resolves a live tracked `RwLock` for this type.
    fn resolve_rwlock() -> impl std::future::Future<Output = Arc<RwLock<Self>>> + Send;

    /// Resolves a live tracked `Mutex` for this type.
    fn resolve_mutex() -> impl std::future::Future<Output = Arc<Mutex<Self>>> + Send;

    /// Resolves the raw initialization result for this provider.
    fn resolve_managed()
    -> impl std::future::Future<Output = std::result::Result<Arc<Self>, crate::ProviderError>> + Send;
}

/// A trait for managed provider types that also support change notifications.
///
/// This capability is required for `Watch(T)` triggers. The default
/// `#[provider]` implementation maps `changed()` to the underlying
/// `StateManager::changed()` notification, so the watch semantics are uniformly
/// defined as: *notify when a new managed-state snapshot is published*.
///
/// Current pre-release behavior: `#[provider]` does not try to defer to manual
/// impls. If you also hand-write `WatchableProvided` for the same type, Rust
/// will emit the normal duplicate-impl compile error.
#[diagnostic::on_unimplemented(
    message = "Watchable Provider required: `{Self}` cannot be used with `Watch(...)` triggers.",
    label = "this trigger requires `{Self}: WatchableProvided`",
    note = "Add `#[provider]` to let the macro generate watch support, or implement `WatchableProvided` manually for `{Self}`."
)]
pub trait WatchableProvided: ManagedProvided {
    /// Returns a future that resolves when a new managed-state snapshot is published.
    fn changed() -> impl std::future::Future<Output = ()> + Send;
}
