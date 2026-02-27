//! # Minimal Example -- `is_shutdown()` Polling Pattern
//!
//! This is the simplest way to use `service-daemon`. It demonstrates:
//! - Defining a service with `#[service]`
//! - Using `is_shutdown()` for graceful exit
//! - Basic dependency injection via `#[provider]`
//! - The interruptible `sleep()` helper
//!
//! **Run**: `cargo run -p example-minimal`
//!
//! > [!WARNING]
//! > Do NOT mix `is_shutdown()` polling with `state()` lifecycle matching
//! > in the same service. These are two independent control-flow paradigms;
//! > mixing them leads to undefined behavior.

use example_minimal as _;
use service_daemon::ServiceDaemon;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(service_daemon::core::logging::DaemonLayer)
        .init();

    let mut daemon = ServiceDaemon::builder().build();
    daemon.run().await;
    daemon.wait().await?;

    Ok(())
}
