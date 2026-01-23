//! Service Daemon Library
//!
//! Provides automatic service management with Type-Based dependency injection
//! and monitoring for Rust applications.
//!
//! # Features
//! - **Automatic Service Management**: Uses `#[service]` to register long-running tasks.
//! - **Event-Driven Triggers**: Use `#[trigger]` for Cron, Broadcast Queue, or Load-Balanced Queue.
//! - **Type-Based DI**: Seamless dependency injection without manual mapping.
//! - **Resilience**: Integrated exponential backoff and graceful shutdown.
//!
//! # Getting Started
//! ```rust
//! use service_daemon::ServiceDaemon;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let daemon = ServiceDaemon::auto_init();
//!     daemon.run().await
//! }
//! ```
//!
//! # Custom Restart Policy
//! ```rust
//! use service_daemon::{ServiceDaemon, RestartPolicy};
//! use std::time::Duration;
//!
//! let policy = RestartPolicy::builder()
//!     .initial_delay(Duration::from_secs(5))
//!     .max_delay(Duration::from_secs(300))
//!     .multiplier(1.5)
//!     .build();
//!
//! let daemon = ServiceDaemon::from_registry_with_policy(policy);
//! ```

pub mod models;
pub mod utils;

// Re-export commonly used items
pub use models::{SERVICE_REGISTRY, ServiceDescription, ServiceEntry, ServiceFn, ServiceParam};
pub use utils::di::Provided;
pub use utils::service_daemon::{
    RestartPolicy, RestartPolicyBuilder, ServiceDaemon, ServiceStatus,
};

// Re-export linkme for use in macro-generated code
pub use linkme;

// Conditionally re-export dependencies based on features
#[cfg(feature = "cron")]
pub use tokio_cron_scheduler;

#[cfg(feature = "uuid-trigger-ids")]
pub use uuid;

// Re-export macros for unified user experience
pub use service_daemon_macro::{provider, service, trigger};
