//! # Triggers Example -- Decoupled Event-Driven Handlers
//!
//! This example demonstrates that **triggers are optional, decoupled components**
//! that can be added to any daemon without modifying existing services.
//!
//! ## Trigger types demonstrated
//! - **Cron**: Fires on a cron schedule via `tokio-cron-scheduler`
//! - **Broadcast Queue (Queue)**: All subscribed handlers receive every message
//! - **Signal (Event/Notify)**: Fire-and-forget notification
//! - **Watch**: Fires when a shared state value changes
//!
//! ## Event flow demo (DI -> trigger chain)
//! ```text
//!                                     +-----------------+
//!                    notify_waiters()  |  UserNotifier   |---> on_tick (Signal)
//!                          +--------> |  (Notify)       |       |
//!                          |          +-----------------+       | tx.send()
//!  +----------------+      |          +-----------------+       |
//!  | event_producer |------+--send()-->|  TaskQueue      |<------+
//!  | (Service)      |      |          |  (Broadcast)    |--> handler_a, handler_b
//!  +----------------+      |          +-----------------+
//!                          |          +-----------------+
//!                          +--send()-->|  WorkerQueue    |--> lb_worker_handler
//!                          |          |  (Queue)        |
//!                          |          +-----------------+
//!                          |          +-----------------+
//!                          +--send()-->|  JobQueue       |--> complex_job_handler
//!                          |          |  (Queue)        |
//!                          |          +-----------------+
//!                          |          +-----------------+
//!                          +--------> |  UserNotifier   |--> on_user_notified
//!                                     |  (Notify)       |    sync_notify_trigger
//!                                     +-----------------+
//! ```
//!
//! **Run**: `RUST_LOG=info cargo run -p example-triggers`

use service_daemon::ServiceDaemon;

// Import library modules so that `#[service]`, `#[trigger]`, and `#[provider]`
// registrations are linked into this binary.
use example_triggers as _;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    service_daemon::core::logging::init_logging();

    let mut daemon = ServiceDaemon::builder().build();
    daemon.run().await;
    daemon.wait().await?;

    Ok(())
}
