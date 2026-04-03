#![deny(unsafe_code)]
//! A declarative Rust framework for automatic service management, event-driven triggers,
//! and type-based dependency injection.
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use service_daemon::prelude::*;
//! use service_daemon::{ServiceDaemon, provider, service, sleep};
//! use tracing::info;
//! use std::sync::Arc;
//!
//! // 1. Define an injectable provider with a default value
//! #[derive(Clone)]
//! #[provider(8080)]
//! pub struct Port(pub i32);
//!
//! // 2. Define a managed service using proc-macros
//! #[service]
//! pub async fn heartbeat_service(port: Arc<Port>) -> anyhow::Result<()> {
//!     while !is_shutdown() {
//!         info!("Service is running on port {}", port);
//!         // Interruptible sleep: returns false if shutdown is requested
//!         if !sleep(std::time::Duration::from_secs(1)).await {
//!             break;
//!         }
//!     }
//!     Ok(())
//! }
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     // 3. Build and run the daemon
//!     let mut daemon = ServiceDaemon::builder().build();
//!     daemon.run().await;
//!     daemon.wait().await?;
//!     Ok(())
//! }
//! ```
//!
//! ## Documentation & Tutorials
//!
//! For the full guide and advanced patterns, visit our components on GitHub:
//!
//! - [**Quick Start Guide**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/quick-start.md) - Complete step-by-step tutorial.
//! - [**Architecture Overview**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/architecture/internal-overview.md) - Deep dive into DI and Registry.

extern crate self as service_daemon;

pub mod core;
pub mod models;

// Re-export commonly used items
pub use core::context::{
    done, is_shutdown, shelve, shelve_clone, sleep, state, trigger_config, unshelve, wait_shutdown,
};
pub use core::di::{ManagedProvided, Provided, WatchableProvided};
pub use core::managed_state::{TrackedNotify, TrackedSender};
pub use core::service_daemon::{
    RestartPolicy, RestartPolicyBuilder, ServiceDaemon, ServiceDaemonBuilder, ServiceDaemonHandle,
};
pub use models::service::{ServicePriority, ServiceScheduling};
pub use models::{
    BackoffController, PROVIDER_REGISTRY, ProviderEntry, ProviderError, ProviderInitError,
    Registry, RegistryBuilder, Result, SERVICE_REGISTRY, ScalingPolicy, ScalingPolicyBuilder,
    ServiceDescription, ServiceEntry, ServiceError, ServiceFn, ServiceId, ServiceParam,
    ServiceStatus, TT, TriggerContext, TriggerHandler, TriggerHost, TriggerMessage,
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

// Re-export log batch size configuration (always available)
pub use core::logging::set_log_batch_size;

// Conditionally re-export file logging utilities
#[cfg(feature = "file-logging")]
pub use core::logging::{FileLogConfig, RotationPolicy, enable_file_logging};

// Re-export diagnostics API (Behavioral Topology)
#[cfg(feature = "diagnostics")]
pub use core::topology_collector::{export_mermaid, reset_topology, start_topology_collector};

// Conditionally re-export dependencies based on features
#[cfg(feature = "cron")]
pub use tokio_cron_scheduler;

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
    pub use crate::core::di::{ManagedProvided, Provided, WatchableProvided};
    pub use crate::models::service::ServicePriority;
    pub use crate::models::service::ServiceScheduling;
    pub use crate::models::service::ServiceStatus;
    pub use crate::models::trigger::TT;
    pub use crate::models::trigger::TT::*;
}
