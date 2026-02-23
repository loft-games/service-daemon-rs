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
//! ```rust,ignore
//! use service_daemon::ServiceDaemon;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let daemon = ServiceDaemon::auto_init();
//!     daemon.run().await?;
//!     Ok(())
//! }
//! ```
//!
//! # Custom Restart Policy
//! ```rust,ignore
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

extern crate self as service_daemon;

pub mod models;
pub mod utils;

// Re-export commonly used items
pub use models::service::ServicePriority;
pub use models::{
    Result, SERVICE_REGISTRY, ServiceDescription, ServiceEntry, ServiceError, ServiceFn,
    ServiceParam, ServiceStatus, TT, TriggerTemplate,
};
pub use std::sync::Arc;
pub use utils::context::{done, is_shutdown, shelve, sleep, state, unshelve, wait_shutdown};
pub use utils::di::Provided;
pub use utils::service_daemon::{
    RestartPolicy, RestartPolicyBuilder, ServiceDaemon, ServiceDaemonHandle,
};

// Re-export simulation utilities
pub use utils::context::try_resolve_mock;
#[cfg(feature = "simulation")]
pub use utils::context::{MockContext, MockContextBuilder};

// Re-export dependencies for use in macro-generated code
pub use futures;
pub use linkme;
pub use tokio;
pub use tokio_util;

// Conditionally re-export file logging utilities
#[cfg(feature = "file-logging")]
pub use utils::logging::{FileLogConfig, enable_file_logging};

// Conditionally re-export dependencies based on features
#[cfg(feature = "cron")]
pub use tokio_cron_scheduler;

#[cfg(feature = "uuid-trigger-ids")]
pub use uuid;

// Re-export macros for unified user experience
pub use service_daemon_macro::{allow_sync, provider, service, trigger};

/// A prelude module for commonly used items and trigger templates.
///
/// Importing this allows using short variant names like `Cron` or `Watch` and
/// provides IDE autocompletion for `#[trigger]` attributes.
pub mod prelude {
    pub use crate::models::service::ServicePriority;
    pub use crate::models::service::ServiceStatus;
    pub use crate::models::trigger::TriggerTemplate;
    pub use crate::models::trigger::TriggerTemplate as TT;
    pub use crate::models::trigger::TriggerTemplate::*;
    pub use crate::utils::context::{is_shutdown, shelve, sleep, state, unshelve, wait_shutdown};
    pub use crate::utils::di::Provided;
}
