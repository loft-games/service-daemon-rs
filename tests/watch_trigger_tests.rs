use service_daemon::{ServiceDaemon, provider, service, trigger};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default, Clone)]
pub struct Counter {
    pub value: u32,
}

#[provider]
pub async fn counter_provider() -> Counter {
    Counter::default()
}

#[service]
pub async fn incrementor(counter: Arc<RwLock<Counter>>) -> anyhow::Result<()> {
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let mut guard = counter.write().await;
    guard.value += 1;
    Ok(())
}

static FIRED: AtomicBool = AtomicBool::new(false);

#[trigger(template = Watch, target = Counter)]
pub async fn watcher(snapshot: Arc<Counter>) -> anyhow::Result<()> {
    if snapshot.value > 0 {
        FIRED.store(true, Ordering::SeqCst);
    }
    Ok(())
}

#[tokio::test]
async fn test_watch_trigger_flow() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt::try_init();
    let daemon = ServiceDaemon::auto_init();

    // Run for a short time
    daemon
        .run_for_duration(std::time::Duration::from_millis(500))
        .await?;

    assert!(
        FIRED.load(Ordering::SeqCst),
        "Watch trigger should have fired after state update"
    );

    Ok(())
}
