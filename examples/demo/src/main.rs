//! # service-daemon-rs — Example Index & Integration Test Hub
//!
//! This crate serves two purposes:
//!
//! ## 1. Example Index
//! The following examples demonstrate different aspects of `service-daemon`:
//!
//! | Example        | Focus                              | Run Command                        |
//! |:---------------|:-----------------------------------|:-----------------------------------|
//! | **minimal**    | `is_shutdown()` polling pattern    | `cargo run -p example-minimal`     |
//! | **complete**   | `state()` lifecycle management     | `cargo run -p example-complete`    |
//! | **triggers**   | Decoupled event-driven triggers    | `cargo run -p example-triggers`    |
//! | **logging**    | File-based JSON log persistence    | `cargo run -p example-logging`     |
//! | **simulation** | `MockContext` for unit testing     | `cargo test -p example-simulation` |
//!
//! > [!WARNING]
//! > Do NOT mix `is_shutdown()` polling (minimal) with `state()` lifecycle
//! > matching (complete) in the same service. These are two independent
//! > control-flow paradigms; mixing them leads to undefined behavior.
//!
//! ## 2. Integration Test Hub
//! Run `cargo test -p service-daemon-demo` to execute cross-cutting
//! integration tests that validate behaviors spanning multiple features.

mod integration_tests;
mod providers;
mod services;
mod triggers;

use crate::providers::trigger_providers::{AsyncConfig, SyncConfig, UserNotifier};
use service_daemon::{Provided, RestartPolicy, ServiceDaemon};
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(service_daemon::utils::logging::DaemonLayer)
        .init();

    tracing::info!("=== service-daemon-rs Example Index ===");
    tracing::info!("This binary runs the full-feature demo. For focused examples, see:");
    tracing::info!("  cargo run -p example-minimal      # Basic is_shutdown() pattern");
    tracing::info!("  cargo run -p example-complete      # Full state() lifecycle");
    tracing::info!("  cargo run -p example-triggers      # Decoupled triggers");
    tracing::info!("  cargo run -p example-logging       # File-based JSON logging");
    tracing::info!("  cargo test -p example-simulation   # MockContext unit tests");
    tracing::info!("Starting full-feature demo...");

    let policy = RestartPolicy::builder()
        .initial_delay(Duration::from_secs(2))
        .max_delay(Duration::from_secs(120))
        .multiplier(1.5)
        .build();

    let daemon = ServiceDaemon::from_registry_with_policy(policy);

    // Demonstration: fire events periodically
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            tracing::info!("--- Firing event notifier from main ---");
            UserNotifier::notify().await;
        }
    });

    // Demonstration: push queue messages
    tokio::spawn(async move {
        let mut job_id = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            let _ = crate::providers::trigger_providers::WorkerQueue::push(format!(
                "LB Work Item #{}",
                job_id
            ))
            .await;
            let _ = crate::providers::trigger_providers::JobQueue::push(
                crate::providers::trigger_providers::ComplexJob {
                    id: job_id,
                    data: format!("Complex Data for job {}", job_id),
                },
            )
            .await;
            job_id += 1;
        }
    });

    // Demonstration: query service status
    let daemon_ref = daemon.handle();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(25)).await;
            let status = daemon_ref.get_service_status("example_service").await;
            tracing::info!("--- [Main] example_service status: {:?} ---", status);
        }
    });

    // Demonstration: log custom providers
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(40)).await;
            let async_cfg = AsyncConfig::resolve().await;
            let sync_cfg = SyncConfig::resolve().await;
            tracing::info!(
                "--- [Main] AsyncConfig initialized at: {:?} ---",
                async_cfg.initialized_at
            );
            tracing::info!("--- [Main] SyncConfig value: {} ---", sync_cfg.value);
        }
    });

    daemon.run().await?;

    Ok(())
}
