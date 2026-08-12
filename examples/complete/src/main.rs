//! # Complete Example -- `state()` Lifecycle Management Pattern
//!
//! This example demonstrates the **advanced lifecycle management** approach:
//! - Using `loop { match state() { ... } }` for explicit state handling
//! - `Recovering` state for crash recovery with `shelve()`/`unshelve()`
//! - `NeedReload` state for graceful context reload
//! - Service priority ordering (`SYSTEM`, `STORAGE`, `EXTERNAL`)
//! - Dependency injection with `Arc<RwLock<T>>` for shared mutable state
//!
//! **Run**: `cargo run -p example-complete`
//!
//! > [!WARNING]
//! > Do NOT mix `is_shutdown()` polling with `state()` lifecycle matching
//! > in the same service. These are two independent control-flow paradigms;
//! > mixing them leads to undefined behavior.

use example_complete as _;
use service_daemon::{RestartPolicy, ServiceDaemon, ServiceError};
use std::time::Duration;
use tracing::{error, info};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    service_daemon::init_logging();

    let policy = RestartPolicy::builder()
        .initial_delay(Duration::from_secs(2))
        .max_delay(Duration::from_secs(30))
        .multiplier(1.5)
        .build();

    let daemon = ServiceDaemon::builder().with_restart_policy(policy).build();

    daemon.run().await;

    match daemon.wait().await {
        Ok(()) => {
            info!("Complete example daemon stopped cleanly");
            Ok(())
        }
        Err(ServiceError::InternalError(message)) => {
            error!(
                reason = %message,
                "Complete example could not install an OS shutdown signal listener"
            );
            Err(ServiceError::InternalError(message).into())
        }
        Err(error) => {
            error!(
                %error,
                "Complete example daemon wait failed outside the documented signal-listener path"
            );
            Err(error.into())
        }
    }
}
