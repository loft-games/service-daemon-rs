use crate::core::managed_state::{Mutex, RwLock};
use std::sync::Arc;

/// A trait for types that can be provided by the DI system.
///
/// This trait is typically implemented by the `#[provider]` macro.
/// If you see a compile error about this trait not being implemented,
/// it means you forgot to add a `#[provider]` for that type.
#[diagnostic::on_unimplemented(
    message = "Missing Provider: The type `{Self}` cannot be injected.",
    label = "this requires `{Self}`, but no `#[provider]` exists for it",
    note = "Add `#[provider]` to a function returning `{Self}`, or use `#[provider]` on the struct definition."
)]
pub trait Provided: 'static + Send + Sync + Clone + Sized {
    /// Resolves a read-only snapshot of this type.
    ///
    /// If promoted to managed state, this returns a consistent snapshot.
    /// Otherwise, it returns the global immutable singleton.
    fn resolve() -> impl std::future::Future<Output = Arc<Self>> + Send;

    /// Resolves a live RwLock for this type (tracked for modifications).
    ///
    /// This is used when a service requests `Arc<RwLock<T>>`.
    fn resolve_rwlock() -> impl std::future::Future<Output = Arc<RwLock<Self>>> + Send {
        async {
            panic!(
                "Type {} does not support RwLock resolution. Did you use #[provider]?",
                std::any::type_name::<Self>()
            )
        }
    }

    /// Resolves a live Mutex for this type (tracked for modifications).
    ///
    /// This is used when a service requests `Arc<Mutex<Self>>`.
    fn resolve_mutex() -> impl std::future::Future<Output = Arc<Mutex<Self>>> + Send {
        async {
            panic!(
                "Type {} does not support Mutex resolution. Did you use #[provider]?",
                std::any::type_name::<Self>()
            )
        }
    }

    /// Returns a future that resolves when the state for this type is modified.
    ///
    /// This is used by the `Watch` trigger template.
    fn changed() -> impl std::future::Future<Output = ()> + Send {
        async {
            panic!(
                "Type {} does not support change notification. Did you use #[provider]?",
                std::any::type_name::<Self>()
            )
        }
    }
}
