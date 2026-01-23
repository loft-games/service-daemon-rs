//! Type-Based Dependency Injection
//!
//! With pure Type-Based DI, all dependencies are resolved at compile time via the
//! `Provided` trait. The `#[provider]` macro automatically implements this trait.

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
pub trait Provided: 'static + Send + Sync + Sized {
    /// Resolves an instance of this type from the DI system.
    ///
    /// This is an async method to allow for non-blocking initialization.
    fn resolve() -> impl std::future::Future<Output = Arc<Self>> + Send;
}
