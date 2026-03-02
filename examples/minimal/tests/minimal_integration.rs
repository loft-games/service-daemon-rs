//! Integration tests for the Minimal example.

use service_daemon::{Registry, RestartPolicy, ServiceDaemon};

/// Verifies that a minimal daemon can start and stop cleanly
/// without any complex lifecycle management.
#[tokio::test]
async fn test_minimal_startup_and_shutdown() -> anyhow::Result<()> {
    let mut daemon = ServiceDaemon::builder()
        .with_registry(Registry::builder().with_tag("__test_isolation__").build())
        .with_restart_policy(RestartPolicy::for_testing())
        .build();
    let cancel = daemon.cancel_token();

    daemon.run().await;

    // Allow services to initialize
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Trigger graceful shutdown
    cancel.cancel();
    daemon.wait().await.unwrap();

    Ok(())
}

// ---------------------------------------------------------------------------
// Test service for is_shutdown() responsiveness
// ---------------------------------------------------------------------------

use std::sync::atomic::{AtomicBool, Ordering};

/// Global flag: set to `true` when the test service exits after `is_shutdown()`.
static SHUTDOWN_EXITED: AtomicBool = AtomicBool::new(false);

/// Test service that polls `is_shutdown()` and sets a global flag on exit.
#[service_daemon::service(tags = ["__test_shutdown__"], priority = 50)]
async fn shutdown_responsive_service() -> anyhow::Result<()> {
    service_daemon::done();
    while !service_daemon::is_shutdown() {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    SHUTDOWN_EXITED.store(true, Ordering::SeqCst);
    Ok(())
}

/// Verifies that `is_shutdown()` becomes true after cancellation,
/// allowing services to exit their polling loops.
#[tokio::test]
async fn test_is_shutdown_responsiveness() -> anyhow::Result<()> {
    SHUTDOWN_EXITED.store(false, Ordering::SeqCst);

    let mut daemon = ServiceDaemon::builder()
        .with_registry(Registry::builder().with_tag("__test_shutdown__").build())
        .with_restart_policy(RestartPolicy::for_testing())
        .build();

    let cancel = daemon.cancel_token();

    daemon.run().await;

    // Wait for service to reach Healthy
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        !SHUTDOWN_EXITED.load(Ordering::SeqCst),
        "Service exited prematurely"
    );

    cancel.cancel();
    daemon.wait().await.unwrap();

    assert!(
        SHUTDOWN_EXITED.load(Ordering::SeqCst),
        "Service did not exit after shutdown signal"
    );
    Ok(())
}
