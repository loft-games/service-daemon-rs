use service_daemon::{ServiceDaemon, provider, service, trigger};
use std::sync::Arc;
use std::time::Duration;

// 1. Sync Provider
#[derive(Clone)]
pub struct SyncData {
    pub value: i32,
}

#[provider]
pub fn provide_sync_data() -> SyncData {
    SyncData { value: 42 }
}

// 2. Sync Service
#[service]
pub fn sync_service(data: Arc<SyncData>) -> anyhow::Result<()> {
    assert_eq!(data.value, 42);
    Ok(())
}

// 3. Sync Trigger
#[provider(default = Notify)]
pub struct SyncSignal;

#[trigger(template = "event", target = SyncSignal)]
pub fn sync_trigger(data: Arc<SyncData>) -> anyhow::Result<()> {
    assert_eq!(data.value, 42);
    Ok(())
}

#[tokio::test]
async fn test_sync_support() -> anyhow::Result<()> {
    let daemon = ServiceDaemon::auto_init();

    // Just run for a short time to ensure services start and don't crash
    let handle = tokio::spawn(async move { daemon.run().await });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Trigger the sync trigger
    SyncSignal::notify().await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    handle.abort();
    Ok(())
}
