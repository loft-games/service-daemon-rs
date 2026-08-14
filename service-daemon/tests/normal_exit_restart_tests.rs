use service_daemon::{
    DiagnosticGenerationExitKind, DiagnosticRestartDecisionKind, Registry, RestartPolicy,
    ServiceDaemon, ServiceStatus, service,
};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

static EXIT_TIMESTAMPS: LazyLock<Mutex<Vec<Instant>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static EXIT_GENERATIONS: AtomicU32 = AtomicU32::new(0);

#[service(tags = ["__test_normal_exit_restart__"])]
async fn normal_exit_service() -> anyhow::Result<()> {
    EXIT_GENERATIONS.fetch_add(1, Ordering::SeqCst);
    EXIT_TIMESTAMPS.lock().await.push(Instant::now());
    service_daemon::done();
    Ok(())
}

#[tokio::test]
async fn test_unexpected_normal_exit_restarts_with_backoff_delay() -> anyhow::Result<()> {
    EXIT_GENERATIONS.store(0, Ordering::SeqCst);
    EXIT_TIMESTAMPS.lock().await.clear();

    let policy = RestartPolicy::builder()
        .initial_delay(Duration::from_millis(60))
        .max_delay(Duration::from_millis(200))
        .multiplier(2.0)
        .jitter_factor(0.0)
        .build();

    let registry = Registry::builder()
        .with_tag("__test_normal_exit_restart__")
        .build();
    assert!(
        registry
            .services()
            .iter()
            .any(|service| service.name() == "normal_exit_service"),
        "normal_exit_service should be materialized"
    );

    let daemon = ServiceDaemon::builder()
        .with_registry(registry)
        .with_restart_policy(policy)
        .build();
    let service_instance = daemon
        .service_instances()
        .into_iter()
        .find(|instance| instance.name() == "normal_exit_service")
        .expect("normal_exit_service should have one instance");

    let cancel = daemon.cancel_token();
    daemon.run().await;

    tokio::time::timeout(Duration::from_secs(2), async {
        while EXIT_GENERATIONS.load(Ordering::SeqCst) < 3 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("normal exit service should restart through backoff");
    cancel.cancel();
    daemon.wait().await?;

    let generations = EXIT_GENERATIONS.load(Ordering::SeqCst);
    assert!(
        generations >= 3,
        "expected at least three generations after normal exit, observed {}",
        generations
    );

    let timestamps = EXIT_TIMESTAMPS.lock().await.clone();
    assert!(
        timestamps.len() >= 3,
        "expected at least three startup timestamps, observed {:?}",
        timestamps.len()
    );

    let first_restart_gap = timestamps[1].duration_since(timestamps[0]);
    let second_restart_gap = timestamps[2].duration_since(timestamps[1]);
    assert!(
        first_restart_gap >= Duration::from_millis(50),
        "normal exit restart should wait for backoff before the second generation: {:?}",
        first_restart_gap
    );
    assert!(
        second_restart_gap > first_restart_gap,
        "normal exit restart delays should increase across consecutive exits: first={:?}, second={:?}",
        first_restart_gap,
        second_restart_gap
    );

    assert_eq!(service_instance.status().await, ServiceStatus::Terminated);

    let service = daemon
        .diagnostics_snapshot()
        .services
        .into_iter()
        .find(|service| service.service_name == "normal_exit_service")
        .expect("normal exit service diagnostics should be recorded");
    assert!(service.aggregate.lifecycle.restart >= 2);
    assert!(service.aggregate.lifecycle.backoff_restart >= 2);
    assert_eq!(
        service.aggregate.lifecycle.last_exit_kind,
        Some(DiagnosticGenerationExitKind::NormalExit)
    );
    assert_eq!(
        service.aggregate.lifecycle.last_restart_decision,
        Some(DiagnosticRestartDecisionKind::BackoffNormalExit)
    );

    Ok(())
}
