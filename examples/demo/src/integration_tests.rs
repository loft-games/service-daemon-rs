//! Consolidated Integration Tests
//!
//! This module contains integration tests grouped by feature area.
//! These tests reuse real implementation types from the application
//! to ensure that the examples provided in the source are functional.

#[cfg(test)]
mod tests {
    use crate::providers::trigger_providers::{ExternalStatus, UserNotifier};
    use crate::providers::typed_providers::GlobalStats;
    use service_daemon::prelude::*;
    use service_daemon::{RestartPolicy, ServiceDaemon};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    // --- 1. Declarative Patterns ---
    #[tokio::test]
    async fn test_declarative_queues_and_signals() {
        let _ = tracing_subscriber::fmt::try_init();
        let daemon = ServiceDaemon::from_registry_with_policy(RestartPolicy::for_testing());
        let cancel = daemon.cancel_token();

        // Check for triggers. Triggers are registered as services.
        let has_lb_worker = service_daemon::SERVICE_REGISTRY
            .iter()
            .any(|e| e.name == "lb_worker_trigger");
        if !has_lb_worker {
            println!("Registry contents:");
            for entry in service_daemon::SERVICE_REGISTRY.iter() {
                println!("  - {}", entry.name);
            }
        }
        assert!(has_lb_worker);

        // Signal test
        let handle = tokio::spawn(async move { daemon.run().await.unwrap() });

        UserNotifier::notify().await;

        // Allow some time for execution
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        cancel.cancel();
        let _ = handle.await;
    }

    // --- 2. Watch Triggers & Promotion ---
    static WATCH_FIRED: AtomicU32 = AtomicU32::new(0);

    #[service_daemon::trigger(template = Watch, target = GlobalStats, priority = 50)]
    pub async fn stats_watcher(snapshot: Arc<GlobalStats>) -> anyhow::Result<()> {
        if !snapshot.last_status.is_empty() {
            WATCH_FIRED.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_watch_and_dynamic_promotion() -> anyhow::Result<()> {
        let daemon = ServiceDaemon::from_registry_with_policy(RestartPolicy::for_testing());
        let cancel = daemon.cancel_token();
        let handle = tokio::spawn(async move { daemon.run().await.unwrap() });

        // Wait longer for services to reach Healthy status (wave startup synchronization)
        tokio::time::sleep(std::time::Duration::from_secs(6)).await;

        // External modification triggers promotion and the watch trigger
        {
            let lock = GlobalStats::rwlock().await;
            let mut guard = lock.write().await;
            guard.last_status = "Updated".to_string();
        }

        // Wait for watch trigger to fire
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(WATCH_FIRED.load(Ordering::SeqCst) >= 1);

        cancel.cancel();
        let _ = handle.await;
        Ok(())
    }

    // --- 3. Zero Lockdown (Non-blocking Snapshots) ---
    #[tokio::test]
    async fn test_zero_lockdown_reads() -> anyhow::Result<()> {
        // Ensure promoted
        let lock = ExternalStatus::rwlock().await;

        let lock_clone = lock.clone();
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let b1 = barrier.clone();

        let writer = tokio::spawn(async move {
            let mut guard = lock_clone.write().await;
            guard.message = "Locked".to_string();
            b1.wait().await;
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        });

        barrier.wait().await;

        let start = std::time::Instant::now();
        let snapshot = ExternalStatus::resolve().await;
        let elapsed = start.elapsed();

        // Must be non-blocking
        assert!(elapsed < std::time::Duration::from_millis(50));
        // The macro initializes named struct fields with Default::default()
        assert_eq!(snapshot.message, "");

        writer.await?;

        let final_snapshot = ExternalStatus::resolve().await;
        assert_eq!(final_snapshot.message, "Locked");

        Ok(())
    }

    // --- 4. Sync Support ---
    #[service_daemon::provider(default = Notify)]
    struct SyncTestSignal;

    #[service_daemon::trigger(template = Event, target = SyncTestSignal, priority = 50)]
    pub fn sync_handler() -> anyhow::Result<()> {
        println!("Sync handler fired!");
        Ok(())
    }

    #[tokio::test]
    async fn test_sync_trigger_support() -> anyhow::Result<()> {
        let daemon = ServiceDaemon::from_registry_with_policy(RestartPolicy::for_testing());
        let cancel = daemon.cancel_token();
        let handle = tokio::spawn(async move { daemon.run().await.unwrap() });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        SyncTestSignal::notify().await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        cancel.cancel();
        let _ = handle.await;
        Ok(())
    }

    #[tokio::test]
    async fn test_ordered_shutdown() -> anyhow::Result<()> {
        use std::sync::Mutex as StdMutex;
        let exit_sequence = Arc::new(StdMutex::new(Vec::new()));

        let mut daemon = ServiceDaemon::new();

        // Priority 0: Takes 100ms to exit
        let seq1 = exit_sequence.clone();
        daemon.register(
            "priority_0",
            Arc::new(move |_| {
                let s = seq1.clone();
                Box::pin(async move {
                    service_daemon::done(); // Signal ready
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

        // Priority 50: Takes 50ms to exit
        let seq2 = exit_sequence.clone();
        daemon.register(
            "priority_50",
            Arc::new(move |_| {
                let s = seq2.clone();
                Box::pin(async move {
                    service_daemon::done(); // Signal ready
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

        // Priority 100: Exit immediately
        let seq3 = exit_sequence.clone();
        daemon.register(
            "priority_100",
            Arc::new(move |_| {
                let s = seq3.clone();
                Box::pin(async move {
                    service_daemon::done(); // Signal ready
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
        // If ordered shutdown works, priority 0 must finish before 50 is even signaled, etc.
        // Sequence MUST be [0, 50, 100].
        assert_eq!(
            final_seq,
            vec![0, 50, 100],
            "Services did not exit in priority order!"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_ordered_startup() -> anyhow::Result<()> {
        use std::sync::Mutex as StdMutex;
        let start_sequence = Arc::new(StdMutex::new(Vec::new()));

        let mut daemon = ServiceDaemon::new();

        // Priority 100
        let seq1 = start_sequence.clone();
        daemon.register(
            "priority_100",
            Arc::new(move |_| {
                let s = seq1.clone();
                Box::pin(async move {
                    s.lock().unwrap().push(100);
                    service_daemon::done(); // Signal ready
                    while !service_daemon::is_shutdown() {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                    Ok(())
                })
            }),
            100,
        );

        // Priority 50
        let seq2 = start_sequence.clone();
        daemon.register(
            "priority_50",
            Arc::new(move |_| {
                let s = seq2.clone();
                Box::pin(async move {
                    s.lock().unwrap().push(50);
                    service_daemon::done(); // Signal ready
                    while !service_daemon::is_shutdown() {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                    Ok(())
                })
            }),
            50,
        );

        // Priority 0
        let seq3 = start_sequence.clone();
        daemon.register(
            "priority_0",
            Arc::new(move |_| {
                let s = seq3.clone();
                Box::pin(async move {
                    s.lock().unwrap().push(0);
                    service_daemon::done(); // Signal ready
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

        // Allow enough time for all waves to start
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        cancel.cancel();
        let _ = handle.await;

        let final_seq = start_sequence.lock().unwrap().clone();
        // Startup order should be descending: 100, 50, 0
        assert_eq!(
            final_seq,
            vec![100, 50, 0],
            "Services did not start in priority order!"
        );

        Ok(())
    }
}
