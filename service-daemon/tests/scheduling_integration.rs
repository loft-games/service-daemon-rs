use service_daemon::{ServiceDaemon, service};
use std::collections::HashSet;
use std::sync::{Arc, LazyLock};
use tokio::sync::Mutex;

static THREAD_NAMES: LazyLock<Arc<Mutex<HashSet<String>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(HashSet::new())));

#[service(scheduling = Isolated)]
async fn isolated_service() -> anyhow::Result<()> {
    let thread_name = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_string();
    THREAD_NAMES.lock().await.insert(thread_name);
    Ok(())
}

#[service(scheduling = Standard)]
async fn standard_service() -> anyhow::Result<()> {
    let thread_name = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_string();
    THREAD_NAMES
        .lock()
        .await
        .insert(format!("standard:{}", thread_name));
    Ok(())
}

#[tokio::test]
async fn test_scheduling_isolation() -> anyhow::Result<()> {
    let mut daemon = ServiceDaemon::builder().build();

    // Start daemon
    daemon.run().await;

    // Wait for services to finish (they are one-offs)
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let names_guard = THREAD_NAMES.lock().await;
    let names: &HashSet<String> = &*names_guard;

    // 1. Verify Isolated service ran in its own named thread
    assert!(
        names.contains("svc-isolated_service"),
        "Isolated service thread name not found in {:?}",
        names
    );

    // 2. Verify Standard service ran (we prefixed it to be sure)
    let has_standard = names.iter().any(|n: &String| n.starts_with("standard:"));
    assert!(
        has_standard,
        "Standard service did not execute, found: {:?}",
        names
    );

    // 3. Optional: Verify standard service did NOT run in the isolated thread
    let standard_in_isolated = names.contains("standard:svc-isolated_service");
    assert!(
        !standard_in_isolated,
        "Standard service should not run in isolated thread"
    );

    Ok(())
}
