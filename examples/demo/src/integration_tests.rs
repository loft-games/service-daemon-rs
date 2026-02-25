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
    use service_daemon::{RestartPolicy, ServiceDaemon, ServiceId};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Helper: Build a ServiceDaemon with ONLY manually constructed service descriptions.
    /// Uses a tag filter that matches nothing to produce an empty auto-registry,
    /// ensuring only the injected services are spawned.
    fn build_daemon_with_services(
        policy: RestartPolicy,
        services: Vec<(ServiceId, &str, service_daemon::ServiceFn, u8)>,
    ) -> ServiceDaemon {
        use service_daemon::tokio_util::sync::CancellationToken;

        let descriptions: Vec<service_daemon::ServiceDescription> = services
            .into_iter()
            .map(
                |(id, name, run, priority)| service_daemon::ServiceDescription {
                    id,
                    name: name.to_string(),
                    run,
                    watcher: None,
                    priority,
                    cancellation_token: CancellationToken::new(),
                    tags: vec![],
                },
            )
            .collect();

        // Use a tag that no real service has, producing an empty auto-registry.
        // Only the manually injected services will run.
        let empty_registry = service_daemon::Registry::builder()
            .with_tag("__test_isolation__")
            .build();

        ServiceDaemon::builder()
            .with_registry(empty_registry)
            .with_restart_policy(policy)
            .with_services(descriptions)
            .build()
    }

    // --- 1. Declarative Patterns ---
    #[tokio::test]
    async fn test_declarative_queues_and_signals() {
        let _ = tracing_subscriber::fmt::try_init();
        let daemon = ServiceDaemon::builder()
            .with_restart_policy(RestartPolicy::for_testing())
            .build();
        let cancel = daemon.cancel_token();

        // Check for triggers. Triggers are registered as services.
        let has_lb_worker = service_daemon::SERVICE_REGISTRY
            .iter()
            .any(|e| e.name == "lb_worker_trigger");
        if !has_lb_worker {
            tracing::info!("Registry contents:");
            for entry in service_daemon::SERVICE_REGISTRY.iter() {
                tracing::info!("  - {}", entry.name);
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

    #[service_daemon::trigger(Watch(GlobalStats), priority = 50)]
    pub async fn stats_watcher(snapshot: Arc<GlobalStats>) -> anyhow::Result<()> {
        if !snapshot.last_status.is_empty() {
            WATCH_FIRED.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_watch_and_dynamic_promotion() -> anyhow::Result<()> {
        let daemon = ServiceDaemon::builder()
            .with_restart_policy(RestartPolicy::for_testing())
            .build();
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

    #[service_daemon::trigger(Event(SyncTestSignal), priority = 50)]
    pub fn sync_handler() -> anyhow::Result<()> {
        tracing::info!("Sync handler fired!");
        Ok(())
    }

    #[tokio::test]
    async fn test_sync_trigger_support() -> anyhow::Result<()> {
        let daemon = ServiceDaemon::builder()
            .with_restart_policy(RestartPolicy::for_testing())
            .build();
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

        // Priority 0: Takes 100ms to exit
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

        // Priority 50: Takes 50ms to exit
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

        // Priority 100: Exit immediately
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

        let daemon = build_daemon_with_services(
            RestartPolicy::for_testing(),
            vec![
                (ServiceId::new(0), "priority_0", fn1, 0),
                (ServiceId::new(1), "priority_50", fn2, 50),
                (ServiceId::new(2), "priority_100", fn3, 100),
            ],
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

        // Priority 100
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

        // Priority 50
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

        // Priority 0
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

        let daemon = build_daemon_with_services(
            RestartPolicy::for_testing(),
            vec![
                (ServiceId::new(0), "priority_100", fn1, 100),
                (ServiceId::new(1), "priority_50", fn2, 50),
                (ServiceId::new(2), "priority_0", fn3, 0),
            ],
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

    // --- 5. Handshake Synchronization ---
    // Verify that lower-priority services wait for higher-priority services to become Healthy.
    #[tokio::test]
    async fn test_handshake_sync_behavior() -> anyhow::Result<()> {
        use std::sync::Mutex as StdMutex;
        let start_log = Arc::new(StdMutex::new(Vec::new()));

        // Higher priority: 100 - sleeps 200ms before done()
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

        // Lower priority: 50 - should not start until high_prio_100 is Healthy
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

        let daemon = build_daemon_with_services(
            RestartPolicy::for_testing(),
            vec![
                (ServiceId::new(0), "high_prio_100", fn1, 100),
                (ServiceId::new(1), "low_prio_50", fn2, 50),
            ],
        );

        let cancel = daemon.cancel_token();
        let handle = tokio::spawn(async move { daemon.run().await.unwrap() });

        // Wait long enough for both waves to complete
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        cancel.cancel();
        let _ = handle.await;

        let log = start_log.lock().unwrap();
        let high_done = log.iter().find(|(s, _)| *s == "high_done");
        let low_start = log.iter().find(|(s, _)| *s == "low_start");

        assert!(high_done.is_some(), "high_prio_100 did not log 'done'");
        assert!(low_start.is_some(), "low_prio_50 did not log 'start'");

        // Verify low_start happened AFTER high_done
        assert!(
            low_start.unwrap().1 >= high_done.unwrap().1,
            "Low priority service started BEFORE high priority service signaled done!"
        );

        Ok(())
    }

    // --- 6. Shelf Persistence Across Crash ---
    // Verify that shelved data survives a service crash and is available in the next generation.
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
                        // Second generation: unshelve and confirm
                        if let Some(v) = service_daemon::unshelve::<u32>("crash_data").await {
                            *rv_clone.lock().unwrap() = Some(v);
                        }
                        service_daemon::done();
                    }
                    _ => {
                        // First generation: shelve and crash
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

        let daemon = build_daemon_with_services(
            RestartPolicy::for_testing(),
            vec![(ServiceId::new(0), "crash_test_service", crash_fn, 50)],
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
}
