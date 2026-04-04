use service_daemon::{Registry, RestartPolicy, ServiceDaemon, service};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

static WAVE_TIMELINE: LazyLock<Mutex<Vec<(&'static str, Instant)>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
static LOW_PRIORITY_STARTED: AtomicBool = AtomicBool::new(false);

#[service(tags = ["__test_wave_timeout__"], priority = 100)]
async fn slow_handshake_service() -> anyhow::Result<()> {
    WAVE_TIMELINE
        .lock()
        .await
        .push(("high_started", Instant::now()));

    tokio::time::sleep(Duration::from_millis(900)).await;
    service_daemon::done();

    while !service_daemon::is_shutdown() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    Ok(())
}

#[service(tags = ["__test_wave_timeout__"], priority = 0)]
async fn low_priority_follower() -> anyhow::Result<()> {
    LOW_PRIORITY_STARTED.store(true, Ordering::SeqCst);
    WAVE_TIMELINE
        .lock()
        .await
        .push(("low_started", Instant::now()));
    service_daemon::done();

    while !service_daemon::is_shutdown() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    Ok(())
}

#[tokio::test]
async fn test_wave_spawn_timeout_allows_next_wave_to_start() -> anyhow::Result<()> {
    LOW_PRIORITY_STARTED.store(false, Ordering::SeqCst);
    WAVE_TIMELINE.lock().await.clear();

    let mut daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__test_wave_timeout__")
                .build(),
        )
        .with_restart_policy(
            RestartPolicy::builder()
                .initial_delay(Duration::from_millis(50))
                .max_delay(Duration::from_millis(200))
                .wave_spawn_timeout(Duration::from_millis(250))
                .wave_stop_timeout(Duration::from_secs(1))
                .build(),
        )
        .build();

    let cancel = daemon.cancel_token();
    let start = Instant::now();
    daemon.run().await;

    tokio::time::sleep(Duration::from_millis(450)).await;
    assert!(
        LOW_PRIORITY_STARTED.load(Ordering::SeqCst),
        "low-priority wave did not start after spawn timeout"
    );

    cancel.cancel();
    daemon.wait().await?;

    let timeline = WAVE_TIMELINE.lock().await.clone();
    let high_started = timeline
        .iter()
        .find(|(name, _)| *name == "high_started")
        .map(|(_, t)| *t);
    let low_started = timeline
        .iter()
        .find(|(name, _)| *name == "low_started")
        .map(|(_, t)| *t);

    assert!(
        high_started.is_some() && low_started.is_some(),
        "expected both waves to record startup: {:?}",
        timeline
    );

    let low_started = low_started.unwrap();
    assert!(
        low_started.duration_since(start) < Duration::from_millis(900),
        "low-priority wave waited for full high-priority handshake instead of timing out"
    );

    Ok(())
}
