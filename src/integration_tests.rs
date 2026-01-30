//! Consolidated Integration Tests
//!
//! This module contains integration tests grouped by feature area.
//! These tests reuse real implementation types from the application
//! to ensure that the examples provided in the source are functional.

#[cfg(test)]
mod tests {
    use crate::providers::trigger_providers::{ExternalStatus, UserNotifier};
    use crate::providers::typed_providers::GlobalStats;
    use service_daemon::{Provided, RestartPolicy, ServiceDaemon};
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
        let handle = tokio::spawn(async move { daemon.run().await });

        UserNotifier::notify().await;

        // Allow some time for execution
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        cancel.cancel();
        let _ = handle.await;
    }

    // --- 2. Watch Triggers & Promotion ---
    static WATCH_FIRED: AtomicU32 = AtomicU32::new(0);

    #[service_daemon::trigger(template = Watch, target = GlobalStats)]
    async fn stats_watcher(snapshot: Arc<GlobalStats>) -> anyhow::Result<()> {
        if !snapshot.last_status.is_empty() {
            WATCH_FIRED.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_watch_and_dynamic_promotion() -> anyhow::Result<()> {
        let daemon = ServiceDaemon::from_registry_with_policy(RestartPolicy::for_testing());
        let cancel = daemon.cancel_token();
        let handle = tokio::spawn(async move { daemon.run().await });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // External modification triggers promotion and the watch trigger
        {
            let lock = GlobalStats::rwlock().await;
            let mut guard = lock.write().await;
            guard.last_status = "Updated".to_string();
        }

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
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

    #[service_daemon::trigger(template = Event, target = SyncTestSignal)]
    fn sync_handler() -> anyhow::Result<()> {
        println!("Sync handler fired!");
        Ok(())
    }

    #[tokio::test]
    async fn test_sync_trigger_support() -> anyhow::Result<()> {
        let daemon = ServiceDaemon::from_registry_with_policy(RestartPolicy::for_testing());
        let cancel = daemon.cancel_token();
        let handle = tokio::spawn(async move { daemon.run().await });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        SyncTestSignal::notify().await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        cancel.cancel();
        let _ = handle.await;
        Ok(())
    }
}
