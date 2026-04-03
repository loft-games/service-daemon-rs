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
use service_daemon::{RestartPolicy, ServiceDaemon};
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    service_daemon::core::logging::init_logging();

    let policy = RestartPolicy::builder()
        .initial_delay(Duration::from_secs(2))
        .max_delay(Duration::from_secs(30))
        .multiplier(1.5)
        .build();

    let mut daemon = ServiceDaemon::builder().with_restart_policy(policy).build();

    daemon.run().await;
    daemon.wait().await?;
    Ok(())
}
