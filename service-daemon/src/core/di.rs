use crate::core::managed_state::{Mutex, RwLock};
use crate::{ProviderError, ProviderInitError};
use std::sync::Arc;

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
/// you forgot to add `#[provider]` for that type or write a manual provider impl.
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
/// Manual impls for the same type conflict with the macro-generated impl and
/// are reported by Rust as duplicate impl errors.
#[diagnostic::on_unimplemented(
    message = "Managed Provider required: `{Self}` cannot be injected as `Arc<RwLock<_>>` or `Arc<Mutex<_>>`.",
    label = "this injection requires `{Self}: ManagedProvided`",
    note = "Add `#[provider]` to let the macro generate managed-state support, or implement `ManagedProvided` manually for `{Self}`."
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

/// A trait for managed provider types that also support change notifications.
///
/// This capability is required for `Watch(T)` triggers. The default
/// `#[provider]` implementation waits on the current effective provider slot's
/// value-change notification and the current daemon scope's binding-change
/// notification. A watch therefore wakes when a managed snapshot is published,
/// or when the daemon switches that provider type to a local override/fork.
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
    /// Returns a future that resolves when the effective slot's value or binding changes.
    fn changed() -> impl std::future::Future<Output = ()> + Send;
}
