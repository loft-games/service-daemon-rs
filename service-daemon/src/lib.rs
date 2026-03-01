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
//!     // Infallible build -- always succeeds
//!     let mut daemon = ServiceDaemon::builder().build();
//!     daemon.run().await;
//!     daemon.wait().await?;
//!     Ok(())
//! }
//! ```
//!
//! # Tag-based Registry
//! ```rust,ignore
//! use service_daemon::{ServiceDaemon, Registry};
//!
//! let reg = Registry::builder().with_tag("infra").build();
//! let mut daemon = ServiceDaemon::builder()
//!     .with_registry(reg)
//!     .build();
//! daemon.run().await;
//! daemon.wait().await?;
//! ```

extern crate self as service_daemon;

pub mod core;
pub mod models;

// Re-export commonly used items
pub use core::context::{
    done, is_shutdown, shelve, shelve_clone, sleep, state, trigger_config, unshelve, wait_shutdown,
};
pub use core::di::Provided;
pub use core::service_daemon::{
    RestartPolicy, RestartPolicyBuilder, ServiceDaemon, ServiceDaemonBuilder, ServiceDaemonHandle,
};
pub use models::service::ServicePriority;
pub use models::{
    BackoffController, Registry, RegistryBuilder, Result, SERVICE_REGISTRY, ScalingPolicy,
    ScalingPolicyBuilder, ServiceDescription, ServiceEntry, ServiceError, ServiceFn, ServiceId,
    ServiceParam, ServiceStatus, TT, TriggerContext, TriggerHandler, TriggerHost, TriggerMessage,
    trigger_clone_payload,
};
pub use std::sync::Arc;

// Re-export simulation utilities (feature-gated toolbox)
#[cfg(feature = "simulation")]
pub use core::context::{MockContext, MockContextBuilder, SimulationHandle};

// Re-export dependencies for use in macro-generated code
pub use futures;
pub use linkme;
pub use tokio;
pub use tokio_util;

// Conditionally re-export file logging utilities
#[cfg(feature = "file-logging")]
pub use core::logging::{FileLogConfig, enable_file_logging};

// Conditionally re-export dependencies based on features
#[cfg(feature = "cron")]
pub use tokio_cron_scheduler;

#[cfg(feature = "uuid-trigger-ids")]
pub use uuid;

// Re-export macros for unified user experience
pub use service_daemon_macro::{provider, service, trigger};

/// A prelude module for commonly used items and trigger templates.
///
/// Importing this allows using short variant names like `Cron` or `Watch` and
/// provides IDE autocompletion for `#[trigger]` attributes.
pub mod prelude {
    pub use crate::core::context::{
        is_shutdown, shelve, shelve_clone, sleep, state, unshelve, wait_shutdown,
    };
    pub use crate::core::di::Provided;
    pub use crate::models::service::ServicePriority;
    pub use crate::models::service::ServiceStatus;
    pub use crate::models::trigger::TT;
    pub use crate::models::trigger::TT::*;
}
