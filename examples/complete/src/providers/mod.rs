//! Type-safe dependency providers for the complete example.
//!
//! Demonstrates various provider patterns:
//! - Simple newtype wrappers with defaults
//! - Environment variable binding
//! - Async initialization
//! - Snapshot injection via `Arc<T>`
//! - Managed state via `Arc<RwLock<T>>` / `Arc<Mutex<T>>`
//! - Watchable state for `Watch(T)` triggers on normal `#[provider]` types
//! - Async fn providers with dependency injection

pub mod fn_providers;
pub mod typed_providers;
