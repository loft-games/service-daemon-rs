//! Integration tests for the Complete Lifecycle example.
//!
//! These tests verify ordered startup/shutdown, crash recovery,
//! handshake synchronization, and zero-lockdown reads.
//!
//! All test services use `#[service(tags = [...])]` with isolated tags
//! to prevent cross-test interference. Global atomics and `std::sync::Mutex`
//! are used for state observation since test assertions run outside the
//! service context.

use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU32, Ordering};

use example_complete::providers::{fn_providers::ConnectionString, typed_providers::GlobalStats};
use service_daemon::{Provided, Registry, RestartPolicy, ServiceDaemon, ServiceStatus};

// ===========================================================================
// Test services for: test_ordered_startup
// ===========================================================================

/// Shared start sequence for ordered startup tests.
static STARTUP_SEQ: std::sync::LazyLock<StdMutex<Vec<u8>>> =
    std::sync::LazyLock::new(|| StdMutex::new(Vec::new()));

/// High-priority test service (priority 100) for ordered startup.
#[service_daemon::service(tags = ["__test_ordered_startup__"], priority = 100)]
async fn startup_service_100() -> anyhow::Result<()> {
    STARTUP_SEQ.lock().unwrap().push(100);
    service_daemon::done();
    while !service_daemon::is_shutdown() {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    Ok(())
}

/// Mid-priority test service (priority 50) for ordered startup.
#[service_daemon::service(tags = ["__test_ordered_startup__"], priority = 50)]
async fn startup_service_50() -> anyhow::Result<()> {
    STARTUP_SEQ.lock().unwrap().push(50);
    service_daemon::done();
    while !service_daemon::is_shutdown() {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    Ok(())
}

/// Low-priority test service (priority 0) for ordered startup.
#[service_daemon::service(tags = ["__test_ordered_startup__"], priority = 0)]
async fn startup_service_0() -> anyhow::Result<()> {
    STARTUP_SEQ.lock().unwrap().push(0);
    service_daemon::done();
    while !service_daemon::is_shutdown() {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    Ok(())
}

/// Verifies that services start in descending priority order (100 -> 50 -> 0).
#[tokio::test]
async fn test_ordered_startup() -> anyhow::Result<()> {
    STARTUP_SEQ.lock().unwrap().clear();

    let mut daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__test_ordered_startup__")
                .build(),
        )
        .with_restart_policy(RestartPolicy::for_testing())
        .build();

    let cancel = daemon.cancel_token();
    daemon.run().await;

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    cancel.cancel();
    daemon.wait().await.unwrap();

    let final_seq = STARTUP_SEQ.lock().unwrap().clone();
    assert_eq!(
        final_seq,
        vec![100, 50, 0],
        "Services did not start in priority order!"
    );

    Ok(())
}

// ===========================================================================
// Test services for: test_ordered_shutdown
// ===========================================================================

/// Shared exit sequence for ordered shutdown tests.
static SHUTDOWN_SEQ: std::sync::LazyLock<StdMutex<Vec<u8>>> =
    std::sync::LazyLock::new(|| StdMutex::new(Vec::new()));

/// Low-priority test service (priority 0) for ordered shutdown.
/// Exits last (highest priority shuts down last -> lowest first).
#[service_daemon::service(tags = ["__test_ordered_shutdown__"], priority = 0)]
async fn shutdown_service_0() -> anyhow::Result<()> {
    service_daemon::done();
    while !service_daemon::is_shutdown() {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    SHUTDOWN_SEQ.lock().unwrap().push(0);
    Ok(())
}

/// Mid-priority test service (priority 50) for ordered shutdown.
#[service_daemon::service(tags = ["__test_ordered_shutdown__"], priority = 50)]
async fn shutdown_service_50() -> anyhow::Result<()> {
    service_daemon::done();
    while !service_daemon::is_shutdown() {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    SHUTDOWN_SEQ.lock().unwrap().push(50);
    Ok(())
}

/// High-priority test service (priority 100) for ordered shutdown.
#[service_daemon::service(tags = ["__test_ordered_shutdown__"], priority = 100)]
async fn shutdown_service_100() -> anyhow::Result<()> {
    service_daemon::done();
    while !service_daemon::is_shutdown() {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    SHUTDOWN_SEQ.lock().unwrap().push(100);
    Ok(())
}

/// Verifies that services shut down in ascending priority order (0 -> 50 -> 100).
#[tokio::test]
async fn test_ordered_shutdown() -> anyhow::Result<()> {
    SHUTDOWN_SEQ.lock().unwrap().clear();

    let mut daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__test_ordered_shutdown__")
                .build(),
        )
        .with_restart_policy(RestartPolicy::for_testing())
        .build();

    let cancel = daemon.cancel_token();
    daemon.run().await;

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    cancel.cancel();
    daemon.wait().await.unwrap();

    let final_seq = SHUTDOWN_SEQ.lock().unwrap().clone();
    assert_eq!(
        final_seq,
        vec![0, 50, 100],
        "Services did not exit in priority order!"
    );

    Ok(())
}

// ===========================================================================
// Test services for: test_shelf_persistence_on_crash
// ===========================================================================

/// Generation counter for the crash test service.
static CRASH_GENERATION: AtomicU32 = AtomicU32::new(0);

/// Recovered value from shelf after crash.
static RECOVERED_VALUE: std::sync::LazyLock<StdMutex<Option<u32>>> =
    std::sync::LazyLock::new(|| StdMutex::new(None));

/// Test service that crashes on first generation, then recovers shelf data.
#[service_daemon::service(tags = ["__test_crash_shelf__"], priority = 50)]
async fn crash_test_service() -> anyhow::Result<()> {
    let generation = CRASH_GENERATION.fetch_add(1, Ordering::SeqCst);
    match service_daemon::state() {
        ServiceStatus::Recovering(_) => {
            if let Some(v) = service_daemon::unshelve::<u32>("crash_data").await {
                *RECOVERED_VALUE.lock().unwrap() = Some(v);
            }
            service_daemon::done();
        }
        _ => {
            service_daemon::shelve("crash_data", 42u32).await;
            if generation == 0 {
                panic!("Simulated crash on first generation!");
            }
            service_daemon::done();
        }
    }
    while !service_daemon::is_shutdown() {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    Ok(())
}

/// Verifies that shelved data survives a service crash and is available
/// in the recovery generation via `unshelve()`.
#[tokio::test]
async fn test_shelf_persistence_on_crash() -> anyhow::Result<()> {
    CRASH_GENERATION.store(0, Ordering::SeqCst);
    *RECOVERED_VALUE.lock().unwrap() = None;

    let mut daemon = ServiceDaemon::builder()
        .with_registry(Registry::builder().with_tag("__test_crash_shelf__").build())
        .with_restart_policy(RestartPolicy::for_testing())
        .build();

    let cancel = daemon.cancel_token();
    daemon.run().await;

    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    cancel.cancel();
    daemon.wait().await.unwrap();

    let value = RECOVERED_VALUE.lock().unwrap().take();
    assert_eq!(value, Some(42), "Shelf data did not survive the crash!");

    Ok(())
}

// ===========================================================================
// Test services for: test_handshake_sync_behavior
// ===========================================================================

/// Shared log for handshake timing assertions.
static HANDSHAKE_LOG: std::sync::LazyLock<StdMutex<Vec<(&'static str, std::time::Instant)>>> =
    std::sync::LazyLock::new(|| StdMutex::new(Vec::new()));

/// High-priority service that delays `done()` by 200ms.
#[service_daemon::service(tags = ["__test_handshake__"], priority = 100)]
async fn handshake_high_prio() -> anyhow::Result<()> {
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    HANDSHAKE_LOG
        .lock()
        .unwrap()
        .push(("high_done", std::time::Instant::now()));
    service_daemon::done();
    while !service_daemon::is_shutdown() {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    Ok(())
}

/// Low-priority service that starts after high-priority handshake.
#[service_daemon::service(tags = ["__test_handshake__"], priority = 50)]
async fn handshake_low_prio() -> anyhow::Result<()> {
    HANDSHAKE_LOG
        .lock()
        .unwrap()
        .push(("low_start", std::time::Instant::now()));
    service_daemon::done();
    while !service_daemon::is_shutdown() {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    Ok(())
}

/// Verifies handshake synchronization: lower-priority services must wait
/// for higher-priority services to signal `done()` before starting.
#[tokio::test]
async fn test_handshake_sync_behavior() -> anyhow::Result<()> {
    HANDSHAKE_LOG.lock().unwrap().clear();

    let mut daemon = ServiceDaemon::builder()
        .with_registry(Registry::builder().with_tag("__test_handshake__").build())
        .with_restart_policy(RestartPolicy::for_testing())
        .build();

    let cancel = daemon.cancel_token();
    daemon.run().await;

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    cancel.cancel();
    daemon.wait().await.unwrap();

    let log = HANDSHAKE_LOG.lock().unwrap();
    let high_done = log.iter().find(|(s, _)| *s == "high_done");
    let low_start = log.iter().find(|(s, _)| *s == "low_start");

    assert!(high_done.is_some(), "high_prio_100 did not log 'done'");
    assert!(low_start.is_some(), "low_prio_50 did not log 'start'");
    assert!(
        low_start.unwrap().1 >= high_done.unwrap().1,
        "Low priority service started BEFORE high priority service signaled done!"
    );

    Ok(())
}

// ===========================================================================
// Tests that do NOT require manually constructed services
// ===========================================================================

/// Verifies that `resolve()` returns a non-blocking snapshot even while
/// a writer holds the RwLock. This guarantees zero-lockdown reads for
/// any service that holds `Arc<T>` (snapshot) rather than `Arc<RwLock<T>>`.
#[tokio::test]
async fn test_zero_lockdown_reads() -> anyhow::Result<()> {
    // Acquire the RwLock (promotes to managed state)
    let lock = GlobalStats::resolve_rwlock().await;
    let lock_clone = lock.clone();

    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let barrier_clone = barrier.clone();

    // Spawn a writer that holds the write lock for 300ms
    let writer = tokio::spawn(async move {
        let mut guard = lock_clone.write().await;
        guard.last_status = "Locked".to_string();
        barrier_clone.wait().await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    });

    // Wait for writer to acquire the lock
    barrier.wait().await;

    // Snapshot read MUST NOT block despite the held write lock
    let start = std::time::Instant::now();
    let snapshot = GlobalStats::resolve().await;
    let elapsed = start.elapsed();

    assert!(
        elapsed < std::time::Duration::from_millis(50),
        "resolve() blocked for {:?} -- expected non-blocking",
        elapsed
    );
    // Snapshot should see the DEFAULT value, not the locked value
    assert_eq!(snapshot.last_status, "");

    writer.await?;

    // After writer releases, next snapshot should see the updated value
    let final_snapshot = GlobalStats::resolve().await;
    assert_eq!(final_snapshot.last_status, "Locked");

    Ok(())
}

/// Verifies that an `async fn` provider with `Arc<T>` parameter injection
/// correctly resolves its dependencies and produces the expected output.
///
/// This test is a **regression guard** for P1 (async fn provider parameter injection).
/// It exercises the full dependency chain at runtime:
///
///   `Port(8080)` + `DbUrl("mysql://localhost")` -> `ConnectionString("mysql://localhost:8080")`
///
/// If the macro fails to generate DI resolution code for function parameters,
/// this test will fail with a type error or incorrect output.
#[tokio::test]
async fn test_fn_provider_dependency_chain() {
    // Resolve the async fn provider - this triggers the full dependency chain.
    let conn_str = ConnectionString::resolve().await;

    // The connection string should be assembled from Port(8080) + DbUrl("mysql://localhost")
    assert_eq!(
        conn_str.0, "mysql://localhost:8080",
        "Async fn provider did not correctly resolve its Arc<T> dependencies. \
         Expected 'mysql://localhost:8080' from Port(8080) + DbUrl(\"mysql://localhost\"), \
         got '{}'",
        conn_str.0
    );
}
