//! Service Daemon Library
//!
//! Provides automatic service management, dependency injection, and monitoring
//! for Rust applications.

pub mod models;
pub mod utils;

// Re-export commonly used items
pub use models::{
    PROVIDER_REGISTRY, ProviderEntry, SERVICE_REGISTRY, ServiceDescription, ServiceEntry,
    ServiceFn, ServiceParam,
};
pub use utils::di::{Container, GLOBAL_CONTAINER};
pub use utils::service_daemon::ServiceDaemon;

// Re-export linkme for use in macro-generated code
pub use linkme;

// Re-export macros for unified user experience
pub use service_daemon_macro::{provider, service, verify_setup};
