//! Type-safe providers using struct-based DI.
//!
//! These providers showcase different initialization strategies:
//! - Literal defaults (`Port`, `DbUrl`)
//! - Environment variable fallback (`DbUrl` with `env`)
//! - Shared mutable state via automatic RwLock promotion (`GlobalStats`)

use service_daemon::provider;
use std::sync::Arc;

/// Server port configuration.
/// Uses `#[provider(8080)]` for compile-time default.
#[derive(Clone)]
#[provider(8080)]
pub struct Port(pub i32);

/// Database URL configuration.
/// Falls back to the `DATABASE_URL` environment variable at runtime;
/// if absent, uses the static default `"mysql://localhost"`.
#[derive(Clone)]
#[provider("mysql://localhost", env = "DATABASE_URL")]
pub struct DbUrl(pub String);

/// Composite application configuration.
/// Automatically resolves `Port` and `DbUrl` from the DI container.
#[allow(dead_code)]
#[derive(Clone)]
#[provider]
pub struct AppConfig {
    pub port: Arc<Port>,
    pub db_url: Arc<DbUrl>,
}

/// Global statistics -- demonstrates automatic managed and watchable state.
///
/// When a service requests `Arc<RwLock<GlobalStats>>` or `Arc<Mutex<GlobalStats>>`,
/// the daemon automatically promotes this value into managed state. Normal
/// `#[provider]` types also remain watchable, so `Watch(GlobalStats)` is valid.
#[derive(Clone, Default)]
#[provider]
pub struct GlobalStats {
    pub total_processed: u32,
    pub last_status: String,
}
