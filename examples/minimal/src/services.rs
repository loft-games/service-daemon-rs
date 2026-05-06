//! Minimal service definitions.
//!
//! Demonstrates the **simplest pattern** for writing a service:
//! - Use `while !is_shutdown()` as the main loop condition.
//! - Use `sleep()` to avoid busy-waiting.
//! - The framework handles `Initializing -> Healthy` transition automatically.

use std::time::Duration;

use crate::providers::{MinimalListener, Port};
use service_daemon::service;
use tracing::info;

/// A service that demonstrates the Listen template.
///
/// It doesn't run a full HTTP server, but shows how to obtain
/// the listener which was bound early during system-init wave.
#[service]
pub async fn listener_service(listener: Arc<MinimalListener>) -> anyhow::Result<()> {
    let l = listener.get();
    info!(
        "Listener service: Port is already bound at {}",
        l.local_addr()?
    );

    while !service_daemon::is_shutdown() {
        if !service_daemon::sleep(Duration::from_secs(10)).await {
            break;
        }
    }
    Ok(())
}

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
        if !service_daemon::sleep(Duration::from_secs(5)).await {
            break;
        }
        info!("Heartbeat: service is alive on port {}", port);
    }

    info!("Heartbeat service shutting down gracefully");
    Ok(())
}
