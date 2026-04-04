use service_daemon::{Registry, RestartPolicy, ServiceDaemon, ServiceId, ServiceStatus, service};
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
async fn test_normal_exit_restarts_without_backoff_delay() -> anyhow::Result<()> {
    EXIT_GENERATIONS.store(0, Ordering::SeqCst);
    EXIT_TIMESTAMPS.lock().await.clear();

    let policy = RestartPolicy::builder()
        .initial_delay(Duration::from_millis(250))
        .max_delay(Duration::from_millis(500))
        .jitter_factor(0.0)
        .build();

    let mut daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__test_normal_exit_restart__")
                .build(),
        )
        .with_restart_policy(policy)
        .build();

    let cancel = daemon.cancel_token();
    daemon.run().await;

    tokio::time::sleep(Duration::from_millis(120)).await;
    cancel.cancel();
    daemon.wait().await?;

    let generations = EXIT_GENERATIONS.load(Ordering::SeqCst);
    assert!(
        generations >= 2,
        "expected at least two generations after normal exit, observed {}",
        generations
    );

    let timestamps = EXIT_TIMESTAMPS.lock().await.clone();
    assert!(
        timestamps.len() >= 2,
        "expected at least two startup timestamps, observed {:?}",
        timestamps.len()
    );

    let restart_gap = timestamps[1].duration_since(timestamps[0]);
    assert!(
        restart_gap < Duration::from_millis(200),
        "normal exit restart waited too long and appears to have used backoff: {:?}",
        restart_gap
    );

    assert_eq!(
        daemon.handle().get_service_status(&ServiceId::new(0)).await,
        ServiceStatus::Terminated
    );

    Ok(())
}
