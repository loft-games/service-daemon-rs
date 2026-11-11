#![deny(unsafe_code)]
//! A declarative Rust framework for automatic service management, event-driven triggers,
//! and type-based dependency injection.
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use service_daemon::prelude::*;
//! use std::sync::Arc;
//! use std::time::Duration;
//!
//! use service_daemon::{ServiceDaemon, provider, service, sleep};
//! use tracing::info;
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
//!         if !sleep(Duration::from_secs(1)).await {
//!             break;
//!         }
//!     }
//!     Ok(())
//! }
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     // 3. Build and run the daemon
//!     let daemon = ServiceDaemon::builder().build();
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
//! - [**Architecture Overview**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/architecture/internal-overview.md) - DI and registry internals.

// Shared declarations keep the library and internal benchmark on the same implementation.
extern crate self as service_daemon;
include!("crate_root.rs");
