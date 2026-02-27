//! Integration tests for the Complete Lifecycle example.
//!
//! These tests verify ordered startup/shutdown, crash recovery,
//! handshake synchronization, and zero-lockdown reads.

use service_daemon::{
    Registry, RestartPolicy, ServiceDaemon, ServiceDescription, ServiceId, ServiceStatus,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// Helper: Build a ServiceDaemon with manually constructed service descriptions.
fn build_daemon_with_services(
    policy: RestartPolicy,
    services: Vec<(ServiceId, &str, service_daemon::ServiceFn, u8)>,
) -> ServiceDaemon {
    use service_daemon::tokio_util::sync::CancellationToken;

    let descriptions: Vec<ServiceDescription> = services
        .into_iter()
        .map(|(id, name, run, priority)| ServiceDescription {
            id,
            name: name.to_string(),
            run,
            watcher: None,
            priority,
            cancellation_token: CancellationToken::new(),
            tags: vec![],
        })
        .collect();

    ServiceDaemon::builder()
        .with_registry(Registry::builder().with_tag("__test_isolation__").build())
        .with_restart_policy(policy)
        .with_services(descriptions)
        .build()
}

/// Verifies that services start in descending priority order (100 -> 50 -> 0).
#[tokio::test]
async fn test_ordered_startup() -> anyhow::Result<()> {
    use std::sync::Mutex as StdMutex;
    let start_sequence = Arc::new(StdMutex::new(Vec::new()));

    let seq1 = start_sequence.clone();
    let fn1: service_daemon::ServiceFn = Arc::new(move |_| {
        let s = seq1.clone();
        Box::pin(async move {
            s.lock().unwrap().push(100);
            service_daemon::done();
            while !service_daemon::is_shutdown() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Ok(())
        })
    });

    let seq2 = start_sequence.clone();
    let fn2: service_daemon::ServiceFn = Arc::new(move |_| {
        let s = seq2.clone();
        Box::pin(async move {
            s.lock().unwrap().push(50);
            service_daemon::done();
            while !service_daemon::is_shutdown() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Ok(())
        })
    });

    let seq3 = start_sequence.clone();
    let fn3: service_daemon::ServiceFn = Arc::new(move |_| {
        let s = seq3.clone();
        Box::pin(async move {
            s.lock().unwrap().push(0);
            service_daemon::done();
            while !service_daemon::is_shutdown() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Ok(())
        })
    });

    let mut daemon = build_daemon_with_services(
        RestartPolicy::for_testing(),
        vec![
            (ServiceId::new(0), "priority_100", fn1, 100),
            (ServiceId::new(1), "priority_50", fn2, 50),
            (ServiceId::new(2), "priority_0", fn3, 0),
        ],
    );

    let cancel = daemon.cancel_token();

    daemon.run().await;

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    cancel.cancel();
    daemon.wait().await.unwrap();

    let final_seq = start_sequence.lock().unwrap().clone();
    assert_eq!(
        final_seq,
        vec![100, 50, 0],
        "Services did not start in priority order!"
    );

    Ok(())
}

/// Verifies that services shut down in ascending priority order (0 -> 50 -> 100).
#[tokio::test]
async fn test_ordered_shutdown() -> anyhow::Result<()> {
    use std::sync::Mutex as StdMutex;
    let exit_sequence = Arc::new(StdMutex::new(Vec::new()));

    let seq1 = exit_sequence.clone();
    let fn1: service_daemon::ServiceFn = Arc::new(move |_| {
        let s = seq1.clone();
        Box::pin(async move {
            service_daemon::done();
            while !service_daemon::is_shutdown() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            s.lock().unwrap().push(0);
            Ok(())
        })
    });

    let seq2 = exit_sequence.clone();
    let fn2: service_daemon::ServiceFn = Arc::new(move |_| {
        let s = seq2.clone();
        Box::pin(async move {
            service_daemon::done();
            while !service_daemon::is_shutdown() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            s.lock().unwrap().push(50);
            Ok(())
        })
    });

    let seq3 = exit_sequence.clone();
    let fn3: service_daemon::ServiceFn = Arc::new(move |_| {
        let s = seq3.clone();
        Box::pin(async move {
            service_daemon::done();
            while !service_daemon::is_shutdown() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            s.lock().unwrap().push(100);
            Ok(())
        })
    });

    let mut daemon = build_daemon_with_services(
        RestartPolicy::for_testing(),
        vec![
            (ServiceId::new(0), "priority_0", fn1, 0),
            (ServiceId::new(1), "priority_50", fn2, 50),
            (ServiceId::new(2), "priority_100", fn3, 100),
        ],
    );

    let cancel = daemon.cancel_token();

    daemon.run().await;

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    cancel.cancel();
    daemon.wait().await.unwrap();

    let final_seq = exit_sequence.lock().unwrap().clone();
    assert_eq!(
        final_seq,
        vec![0, 50, 100],
        "Services did not exit in priority order!"
    );

    Ok(())
}

/// Verifies that shelved data survives a service crash and is available
/// in the recovery generation via `unshelve()`.
#[tokio::test]
async fn test_shelf_persistence_on_crash() -> anyhow::Result<()> {
    use std::sync::Mutex as StdMutex;
    static GENERATION_COUNTER: AtomicU32 = AtomicU32::new(0);
    let recovered_value = Arc::new(StdMutex::new(None::<u32>));

    let rv = recovered_value.clone();
    let crash_fn: service_daemon::ServiceFn = Arc::new(move |_| {
        let rv_clone = rv.clone();
        Box::pin(async move {
            let generation = GENERATION_COUNTER.fetch_add(1, Ordering::SeqCst);
            match service_daemon::state() {
                ServiceStatus::Recovering(_) => {
                    if let Some(v) = service_daemon::unshelve::<u32>("crash_data").await {
                        *rv_clone.lock().unwrap() = Some(v);
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
        })
    });

    let mut daemon = build_daemon_with_services(
        RestartPolicy::for_testing(),
        vec![(ServiceId::new(0), "crash_test_service", crash_fn, 50)],
    );

    let cancel = daemon.cancel_token();

    daemon.run().await;

    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    cancel.cancel();
    daemon.wait().await.unwrap();

    let value = recovered_value.lock().unwrap().take();
    assert_eq!(value, Some(42), "Shelf data did not survive the crash!");

    Ok(())
}

/// Verifies handshake synchronization: lower-priority services must wait
/// for higher-priority services to signal `done()` before starting.
#[tokio::test]
async fn test_handshake_sync_behavior() -> anyhow::Result<()> {
    use std::sync::Mutex as StdMutex;
    let start_log = Arc::new(StdMutex::new(Vec::new()));

    let log1 = start_log.clone();
    let fn1: service_daemon::ServiceFn = Arc::new(move |_| {
        let l = log1.clone();
        Box::pin(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            l.lock()
                .unwrap()
                .push(("high_done", std::time::Instant::now()));
            service_daemon::done();
            while !service_daemon::is_shutdown() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Ok(())
        })
    });

    let log2 = start_log.clone();
    let fn2: service_daemon::ServiceFn = Arc::new(move |_| {
        let l = log2.clone();
        Box::pin(async move {
            l.lock()
                .unwrap()
                .push(("low_start", std::time::Instant::now()));
            service_daemon::done();
            while !service_daemon::is_shutdown() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Ok(())
        })
    });

    let mut daemon = build_daemon_with_services(
        RestartPolicy::for_testing(),
        vec![
            (ServiceId::new(0), "high_prio_100", fn1, 100),
            (ServiceId::new(1), "low_prio_50", fn2, 50),
        ],
    );

    let cancel = daemon.cancel_token();

    daemon.run().await;

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    cancel.cancel();
    daemon.wait().await.unwrap();

    let log = start_log.lock().unwrap();
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

/// Verifies that `resolve()` returns a non-blocking snapshot even while
/// a writer holds the RwLock. This guarantees zero-lockdown reads for
/// any service that holds `Arc<T>` (snapshot) rather than `Arc<RwLock<T>>`.
#[tokio::test]
async fn test_zero_lockdown_reads() -> anyhow::Result<()> {
    use example_complete::providers::typed_providers::GlobalStats;
    use service_daemon::Provided;

    // Acquire the RwLock (promotes to managed state)
    let lock = GlobalStats::rwlock().await;
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
