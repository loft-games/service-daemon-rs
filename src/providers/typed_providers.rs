//! Type-safe providers using struct-based DI
//!
//! These providers use wrapper types and #[provider] on structs
//! to enable compile-time dependency verification.

use service_daemon::provider;
use std::sync::Arc;

// A wrapper type for the server port configuration.
// Using #[provider(default = X)] auto-generates Deref, Display, and Default.
#[derive(Clone)]
#[provider(default = 8080)]
pub struct Port(pub i32);

// A wrapper type for database URL configuration.
// Using env_name with default fallback
// Note: String literals are automatically expanded to .to_owned() for String fields!
#[derive(Clone)]
#[provider(default = "mysql://localhost", env_name = "DATABASE_URL")]
pub struct DbUrl(pub String);

// A service that depends on Port and DbUrl.
// If either dependency is missing, compilation will fail!
#[allow(dead_code)]
#[derive(Clone)]
#[provider]
pub struct AppConfig {
    pub port: Arc<Port>,
    pub db_url: Arc<DbUrl>,
}

/// Global statistics for the application.
/// This will be automatically promoted to Managed State if any service
/// requests Arc<RwLock<GlobalStats>> or Arc<Mutex<GlobalStats>>.
#[derive(Clone, Default)]
#[provider]
pub struct GlobalStats {
    pub total_processed: u32,
    pub last_status: String,
}
