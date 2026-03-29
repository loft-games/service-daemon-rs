//! Async function providers with dependency injection.
//!
//! Demonstrates `#[provider]` on `async fn` with `Arc<T>` parameters that
//! are automatically resolved from the DI container before the function is called.

use service_daemon::provider;
use std::sync::Arc;

use super::typed_providers::{DbUrl, Port};

/// A composite connection string assembled from `Port` and `DbUrl` providers.
///
/// This provider demonstrates the async fn dependency injection chain:
/// `Port` + `DbUrl` -> `ConnectionString` (via async fn with Arc params).
#[derive(Clone)]
pub struct ConnectionString(pub String);

/// Async fn provider with parameter injection.
///
/// The framework automatically resolves `Arc<Port>` and `Arc<DbUrl>` before
/// calling this function. This is the recommended pattern for providers that
/// depend on other providers and require custom initialization logic.
#[provider]
pub async fn connection_string_provider(port: Arc<Port>, db_url: Arc<DbUrl>) -> ConnectionString {
    ConnectionString(format!("{}:{}", db_url, port))
}
