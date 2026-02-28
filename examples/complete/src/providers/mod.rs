//! Type-safe dependency providers for the complete example.
//!
//! Demonstrates various provider patterns:
//! - Simple newtype wrappers with defaults
//! - Environment variable binding
//! - Async initialization
//! - Shared mutable state (GlobalStats with RwLock promotion)
//! - Async fn providers with dependency injection

pub mod fn_providers;
pub mod typed_providers;
