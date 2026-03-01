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

/// Verifies that `is_shutdown()` becomes true after cancellation,
/// allowing services to exit their polling loops.
#[tokio::test]
async fn test_is_shutdown_responsiveness() -> anyhow::Result<()> {
    use service_daemon::tokio_util::sync::CancellationToken;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let exited = Arc::new(AtomicBool::new(false));
    let exited_clone = exited.clone();

    let shutdown_fn: service_daemon::ServiceFn = Arc::new(move |_| {
        let flag = exited_clone.clone();
        Box::pin(async move {
            service_daemon::done();
            while !service_daemon::is_shutdown() {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            flag.store(true, Ordering::SeqCst);
            Ok(())
        })
    });

    let mut daemon = ServiceDaemon::builder()
        .with_registry(Registry::builder().with_tag("__test_isolation__").build())
        .with_restart_policy(RestartPolicy::for_testing())
        .with_service(service_daemon::ServiceDescription {
            id: service_daemon::ServiceId::new(0),
            name: "shutdown_test".to_string(),
            run: shutdown_fn,
            watcher: None,
            priority: 50,
            cancellation_token: CancellationToken::new(),
            tags: &[],
        })
        .build();

    let cancel = daemon.cancel_token();

    daemon.run().await;

    // Wait for service to reach Healthy
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(!exited.load(Ordering::SeqCst), "Service exited prematurely");

    cancel.cancel();
    daemon.wait().await.unwrap();

    assert!(
        exited.load(Ordering::SeqCst),
        "Service did not exit after shutdown signal"
    );
    Ok(())
}
