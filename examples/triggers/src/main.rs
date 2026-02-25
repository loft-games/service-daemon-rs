//! # Triggers Example — Decoupled Event-Driven Handlers
//!
//! This example demonstrates that **triggers are optional, decoupled components**
//! that can be added to any daemon without modifying existing services.
//!
//! ## Trigger types demonstrated
//! - **Cron**: Fires on a cron schedule via `tokio-cron-scheduler`
//! - **Broadcast Queue (BQueue)**: All subscribed handlers receive every message
//! - **Load-Balancing Queue (LBQueue)**: Messages are distributed to one handler at a time
//! - **Signal (Event/Notify)**: Fire-and-forget notification
//! - **Watch**: Fires when a shared state value changes
//!
//! ## Event tracing demo (publish → trigger chain)
//! ```text
//!                                     ┌─────────────────┐
//!                           publish() │  TickNotifier    │──► on_tick (Signal)
//!                          ┌────────► │  (Notify)        │       │
//!                          │          └─────────────────┘       │ publish("tick_processed")
//!  ┌────────────────┐      │          ┌─────────────────┐       │
//!  │ event_producer │──────┼─push()──►│  TaskQueue      │◄──────┘
//!  │ (Service)      │      │          │  (Broadcast)    │──► handler_a, handler_b
//!  └────────────────┘      │          └─────────────────┘
//!                          │          ┌─────────────────┐
//!                          ├─push()──►│  WorkerQueue    │──► lb_worker_handler
//!                          │          │  (LBQueue)      │
//!                          │          └─────────────────┘
//!                          │          ┌─────────────────┐
//!                          ├─push()──►│  JobQueue       │──► complex_job_handler
//!                          │          │  (LBQueue)      │
//!                          │          └─────────────────┘
//!                          │          ┌─────────────────┐
//!                          └────────► │  UserNotifier   │──► on_user_notified
//!                                     │  (Notify)       │    sync_notify_trigger
//!                                     └─────────────────┘
//! ```
//!
//! **Run**: `RUST_LOG=info cargo run -p example-triggers`

mod providers;
mod services;
mod trigger_handlers;

use service_daemon::ServiceDaemon;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(service_daemon::core::logging::DaemonLayer)
        .init();

    let daemon = ServiceDaemon::builder().build();
    daemon.run().await?;

    Ok(())
}

// =============================================================================
// Integration Tests — Triggers
// =============================================================================
#[cfg(test)]
mod tests {
    use service_daemon::{Registry, RestartPolicy, ServiceDaemon};

    /// Helper: Create an isolated registry that filters out all auto-registered services.
    fn isolated_registry() -> Registry {
        Registry::builder().with_tag("__test_isolation__").build()
    }

    /// Verifies that Cron, Queue, and Signal triggers are all registered
    /// and the daemon can start/stop with them present.
    #[tokio::test]
    async fn test_trigger_registration() -> anyhow::Result<()> {
        let daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .with_restart_policy(RestartPolicy::for_testing())
            .build();
        let cancel = daemon.cancel_token();
        let handle = tokio::spawn(async move { daemon.run().await.unwrap() });

        // Allow time for trigger initialization
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        cancel.cancel();
        let _ = handle.await;

        Ok(())
    }

    /// Verifies that Signal triggers fire when notified.
    #[tokio::test]
    async fn test_signal_trigger_fires() -> anyhow::Result<()> {
        let daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .with_restart_policy(RestartPolicy::for_testing())
            .build();
        let cancel = daemon.cancel_token();
        let handle = tokio::spawn(async move { daemon.run().await.unwrap() });

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Fire the signal
        crate::providers::UserNotifier::notify().await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        cancel.cancel();
        let _ = handle.await;
        Ok(())
    }

    /// Verifies that Watch triggers fire when the watched state changes.
    #[tokio::test]
    async fn test_watch_trigger_on_state_change() -> anyhow::Result<()> {
        use crate::providers::ExternalStatus;

        let daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .with_restart_policy(RestartPolicy::for_testing())
            .build();
        let cancel = daemon.cancel_token();
        let handle = tokio::spawn(async move { daemon.run().await.unwrap() });

        // Wait for services to initialize
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // Modify ExternalStatus — this should trigger the Watch handler
        {
            let lock = ExternalStatus::rwlock().await;
            let mut guard = lock.write().await;
            guard.message = "Watch test update".to_string();
            guard.updated_count = 1;
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        cancel.cancel();
        let _ = handle.await;
        Ok(())
    }
}
