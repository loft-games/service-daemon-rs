//! Type-safe providers using struct-based DI
//!
//! These providers use wrapper types and #[provider] on structs
//! to enable compile-time dependency verification.

use service_daemon::provider;
use std::sync::Arc;

// A wrapper type for the server port configuration.
// Using #[provider(default = X)] auto-generates Deref, Display, and Default.
#[provider(default = 8080)]
pub struct Port(pub i32);

// A wrapper type for database URL configuration.
// Using env_name with default fallback
#[provider(default = "mysql://localhost".to_string(), env_name = "DATABASE_URL")]
pub struct DbUrl(pub String);

// A service that depends on Port and DbUrl.
// If either dependency is missing, compilation will fail!
#[provider]
pub struct AppConfig {
    pub port: Arc<Port>,
    pub db_url: Arc<DbUrl>,
}
