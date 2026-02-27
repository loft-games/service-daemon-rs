//! Minimal service definitions.
//!
//! Demonstrates the **simplest pattern** for writing a service:
//! - Use `while !is_shutdown()` as the main loop condition.
//! - Use `sleep()` to avoid busy-waiting.
//! - The framework handles `Initializing -> Healthy` transition automatically.

use crate::providers::Port;
use service_daemon::service;
use tracing::info;

/// A heartbeat service that logs periodically until shutdown.
///
/// This is the recommended starting point for most services.
/// The `#[service]` macro automatically:
/// 1. Registers this function in the global service registry.
/// 2. Resolves `Port` from the DI container at startup.
/// 3. Wraps the function body in proper lifecycle management.
#[service]
pub async fn heartbeat_service(port: Arc<Port>) -> anyhow::Result<()> {
    info!("Heartbeat service started on port {}", port);

    while !service_daemon::is_shutdown() {
        // `sleep()` is interruptible -- it returns `false` if a shutdown
        // signal arrives during the sleep, allowing immediate exit.
        if !service_daemon::sleep(std::time::Duration::from_secs(5)).await {
            break;
        }
        info!("Heartbeat: service is alive on port {}", port);
    }

    info!("Heartbeat service shutting down gracefully");
    Ok(())
}
