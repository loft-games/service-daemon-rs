use service_daemon::{service, ServiceDaemon};
use std::collections::HashSet;
use std::sync::{Arc, LazyLock};
use tokio::sync::Mutex;

static THREAD_NAMES: LazyLock<Arc<Mutex<HashSet<String>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(HashSet::new())));

async fn record_thread_name(prefix: &str) -> anyhow::Result<()> {
    let thread_name = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_string();
    THREAD_NAMES
        .lock()
        .await
        .insert(format!("{}:{}", prefix, thread_name));
    Ok(())
}

#[service(scheduling = Isolated)]
async fn isolated_service() -> anyhow::Result<()> {
    record_thread_name("isolated").await
}

#[service(scheduling = Standard)]
async fn standard_service() -> anyhow::Result<()> {
    record_thread_name("standard").await
}

#[service(scheduling = HighPriority)]
async fn high_priority_service() -> anyhow::Result<()> {
    record_thread_name("high_priority").await
}

#[tokio::test]
async fn test_scheduling_isolation() -> anyhow::Result<()> {
    THREAD_NAMES.lock().await.clear();

    let mut daemon = ServiceDaemon::builder().build();

    daemon.run().await;

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    std::mem::forget(daemon);

    let names_guard = THREAD_NAMES.lock().await;
    let names: &HashSet<String> = &*names_guard;

    assert!(
        names
            .iter()
            .any(|n| n == "isolated:svc-isolated_service"),
        "Isolated service thread name not found in {:?}",
        names
    );

    assert!(
        names.iter().any(|n| n.starts_with("standard:")),
        "Standard service did not execute, found: {:?}",
        names
    );

    assert!(
        names
            .iter()
            .any(|n| n.starts_with("high_priority:svc-high-priority")),
        "HighPriority service did not execute on the shared high-priority runtime, found: {:?}",
        names
    );

    assert!(
        !names.contains("standard:svc-isolated_service"),
        "Standard service should not run in isolated thread"
    );

    assert!(
        !names.contains("high_priority:svc-isolated_service"),
        "HighPriority service should not run in isolated thread"
    );

    assert!(
        !names.iter().any(|n| n.starts_with("high_priority:standard:")),
        "HighPriority service should not collapse to Standard thread naming, found: {:?}",
        names
    );

    Ok(())
}
