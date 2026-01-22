//! Service Daemon Library
//!
//! Provides automatic service management with Type-Based dependency injection
//! and monitoring for Rust applications.

pub mod models;
pub mod utils;

// Re-export commonly used items
pub use models::{
    SERVICE_REGISTRY, ServiceDescription, ServiceEntry, ServiceFn, ServiceParam, TRIGGER_REGISTRY,
    TriggerEntry,
};
pub use utils::di::Provided;
pub use utils::service_daemon::ServiceDaemon;

// Re-export linkme and other dependencies for use in macro-generated code
pub use linkme;
pub use tokio_cron_scheduler;
pub use uuid;

// Re-export macros for unified user experience
pub use service_daemon_macro::{provider, service, trigger, verify_setup};
