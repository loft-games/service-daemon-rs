//! # Complete Example — `state()` Lifecycle Management Pattern
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

mod providers;
mod services;

use service_daemon::{RestartPolicy, ServiceDaemon};
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Initialize tracing with DaemonLayer
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(service_daemon::utils::logging::DaemonLayer)
        .init();

    // 2. Configure a custom restart policy for crash recovery demonstration.
    //    In production, use longer delays; here we keep them short for visibility.
    let policy = RestartPolicy::builder()
        .initial_delay(Duration::from_secs(2))
        .max_delay(Duration::from_secs(30))
        .multiplier(1.5)
        .build();

    // 3. Create daemon with all auto-registered services
    let daemon = ServiceDaemon::from_registry_with_policy(policy);

    // 4. Run daemon (blocks until Ctrl+C or SIGTERM)
    daemon.run().await?;

    Ok(())
}

// =============================================================================
// Integration Tests — Complete Lifecycle
// =============================================================================
#[cfg(test)]
mod tests {
    use service_daemon::{RestartPolicy, ServiceDaemon, ServiceStatus};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Verifies that services start in descending priority order (100 → 50 → 0).
    #[tokio::test]
    async fn test_ordered_startup() -> anyhow::Result<()> {
        use std::sync::Mutex as StdMutex;
        let start_sequence = Arc::new(StdMutex::new(Vec::new()));

        let mut daemon = ServiceDaemon::new();

        // Priority 100 (starts first)
        let seq1 = start_sequence.clone();
        daemon.register(
            "priority_100",
            Arc::new(move |_| {
                let s = seq1.clone();
                Box::pin(async move {
                    s.lock().unwrap().push(100);
                    service_daemon::done();
                    while !service_daemon::is_shutdown() {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                    Ok(())
                })
            }),
            100,
        );

        // Priority 50 (starts second)
        let seq2 = start_sequence.clone();
        daemon.register(
            "priority_50",
            Arc::new(move |_| {
                let s = seq2.clone();
                Box::pin(async move {
                    s.lock().unwrap().push(50);
                    service_daemon::done();
                    while !service_daemon::is_shutdown() {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                    Ok(())
                })
            }),
            50,
        );

        // Priority 0 (starts last)
        let seq3 = start_sequence.clone();
        daemon.register(
            "priority_0",
            Arc::new(move |_| {
                let s = seq3.clone();
                Box::pin(async move {
                    s.lock().unwrap().push(0);
                    service_daemon::done();
                    while !service_daemon::is_shutdown() {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                    Ok(())
                })
            }),
            0,
        );

        let cancel = daemon.cancel_token();
        let handle = tokio::spawn(async move { daemon.run().await.unwrap() });

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        cancel.cancel();
        let _ = handle.await;

        let final_seq = start_sequence.lock().unwrap().clone();
        assert_eq!(
            final_seq,
            vec![100, 50, 0],
            "Services did not start in priority order!"
        );

        Ok(())
    }

    /// Verifies that services shut down in ascending priority order (0 → 50 → 100).
    #[tokio::test]
    async fn test_ordered_shutdown() -> anyhow::Result<()> {
        use std::sync::Mutex as StdMutex;
        let exit_sequence = Arc::new(StdMutex::new(Vec::new()));

        let mut daemon = ServiceDaemon::new();

        // Priority 0: takes 100ms to exit
        let seq1 = exit_sequence.clone();
        daemon.register(
            "priority_0",
            Arc::new(move |_| {
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
            }),
            0,
        );

        // Priority 50: takes 50ms to exit
        let seq2 = exit_sequence.clone();
        daemon.register(
            "priority_50",
            Arc::new(move |_| {
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
            }),
            50,
        );

        // Priority 100: exits immediately
        let seq3 = exit_sequence.clone();
        daemon.register(
            "priority_100",
            Arc::new(move |_| {
                let s = seq3.clone();
                Box::pin(async move {
                    service_daemon::done();
                    while !service_daemon::is_shutdown() {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                    s.lock().unwrap().push(100);
                    Ok(())
                })
            }),
            100,
        );

        let cancel = daemon.cancel_token();
        let handle = tokio::spawn(async move { daemon.run().await.unwrap() });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        cancel.cancel();
        let _ = handle.await;

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

        let mut daemon = ServiceDaemon::with_restart_policy(RestartPolicy::for_testing());
        let rv = recovered_value.clone();
        daemon.register(
            "crash_test_service",
            Arc::new(move |_| {
                let rv_clone = rv.clone();
                Box::pin(async move {
                    let generation = GENERATION_COUNTER.fetch_add(1, Ordering::SeqCst);
                    match service_daemon::state() {
                        ServiceStatus::Recovering(_) => {
                            // Second generation: recover shelved data
                            if let Some(v) = service_daemon::unshelve::<u32>("crash_data").await {
                                *rv_clone.lock().unwrap() = Some(v);
                            }
                            service_daemon::done();
                        }
                        _ => {
                            // First generation: shelve data and crash
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
            }),
            50,
        );

        let cancel = daemon.cancel_token();
        let handle = tokio::spawn(async move { daemon.run().await.unwrap() });

        // Wait for restart cycle to complete
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        cancel.cancel();
        let _ = handle.await;

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
        let mut daemon = ServiceDaemon::new();

        // Higher priority (100): sleeps 200ms before done()
        let log1 = start_log.clone();
        daemon.register(
            "high_prio_100",
            Arc::new(move |_| {
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
            }),
            100,
        );

        // Lower priority (50): should not start until high_prio_100 is Healthy
        let log2 = start_log.clone();
        daemon.register(
            "low_prio_50",
            Arc::new(move |_| {
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
            }),
            50,
        );

        let cancel = daemon.cancel_token();
        let handle = tokio::spawn(async move { daemon.run().await.unwrap() });

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        cancel.cancel();
        let _ = handle.await;

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
}
