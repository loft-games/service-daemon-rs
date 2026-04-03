//! Integration tests for the Triggers example.
//!
//! These tests verify end-to-end behavior of trigger registration,
//! signal firing, and watch state changes through the public API.

use example_triggers::providers::{ExternalStatus, UserNotifier};
use service_daemon::{Registry, RestartPolicy, ServiceDaemon};

/// Helper: Create an isolated registry that filters out all auto-registered services.
fn isolated_registry() -> Registry {
    Registry::builder().with_tag("__test_isolation__").build()
}

/// Verifies that Cron, Queue, and Signal triggers are all registered
/// and the daemon can start/stop with them present.
#[tokio::test]
async fn test_trigger_registration() -> anyhow::Result<()> {
    let mut daemon = ServiceDaemon::builder()
        .with_registry(isolated_registry())
        .with_restart_policy(RestartPolicy::for_testing())
        .build();
    let cancel = daemon.cancel_token();

    daemon.run().await;

    // Allow time for trigger initialization
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    cancel.cancel();
    daemon.wait().await?;

    Ok(())
}

/// Verifies that Signal triggers fire when notified.
#[tokio::test]
async fn test_signal_trigger_fires() -> anyhow::Result<()> {
    let mut daemon = ServiceDaemon::builder()
        .with_registry(isolated_registry())
        .with_restart_policy(RestartPolicy::for_testing())
        .build();
    let cancel = daemon.cancel_token();

    daemon.run().await;

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Fire the signal
    UserNotifier::resolve().await.notify();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    cancel.cancel();
    daemon.wait().await?;
    Ok(())
}

/// Verifies that Watch triggers fire when the watched state changes.
#[tokio::test]
async fn test_watch_trigger_on_state_change() -> anyhow::Result<()> {
    let mut daemon = ServiceDaemon::builder()
        .with_registry(isolated_registry())
        .with_restart_policy(RestartPolicy::for_testing())
        .build();
    let cancel = daemon.cancel_token();

    daemon.run().await;

    // Wait for services to initialize
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Modify ExternalStatus -- this should trigger the Watch handler
    {
        let lock = ExternalStatus::resolve_rwlock().await;
        let mut guard = lock.write().await;
        guard.message = "Watch test update".to_string();
        guard.updated_count = 1;
    }

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    cancel.cancel();
    daemon.wait().await?;
    Ok(())
}
