//! `#[allow_sync]` attribute macro implementation.
//!
//! This macro marks a synchronous function as intentionally not needing `async`.

use proc_macro::TokenStream;

/// Marks a synchronous function as intentionally not needing `async`.
///
/// Use this attribute to suppress warnings about synchronous functions
/// blocking the async executor. Only use this when you are certain that
/// the synchronous function will not perform blocking I/O or long-running
/// computations.
///
/// # Example
/// ```rust,ignore
/// use service_daemon::{service, allow_sync};
///
/// #[allow_sync]
/// #[service]
/// pub fn my_fast_sync_service() -> anyhow::Result<()> {
///     // This function is intentionally sync and won't block.
///     Ok(())
/// }
/// ```
pub fn allow_sync_impl(_attr: TokenStream, item: TokenStream) -> TokenStream {
    // This is a no-op macro. It just marks the function.
    // The actual logic is handled by #[service], #[trigger], and #[provider].
    item
}
