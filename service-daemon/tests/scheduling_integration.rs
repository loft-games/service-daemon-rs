use service_daemon::{Registry, ServiceDaemon, service};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tokio::sync::Mutex;

static THREAD_NAMES: LazyLock<Arc<Mutex<HashSet<String>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(HashSet::new())));
static STANDARD_STARTED: AtomicBool = AtomicBool::new(false);
static STANDARD_STOPPED: AtomicBool = AtomicBool::new(false);
static HIGH_PRIORITY_STARTED: AtomicBool = AtomicBool::new(false);
static HIGH_PRIORITY_STOPPED: AtomicBool = AtomicBool::new(false);
static ISOLATED_STARTED: AtomicBool = AtomicBool::new(false);
static ISOLATED_STOPPED: AtomicBool = AtomicBool::new(false);

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

#[service(tags = ["__test_scheduling_threads__"], scheduling = Isolated)]
async fn isolated_service() -> anyhow::Result<()> {
    record_thread_name("isolated").await?;
    service_daemon::done();

    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }

    Ok(())
}

#[service(tags = ["__test_scheduling_threads__"], scheduling = Standard)]
async fn standard_service() -> anyhow::Result<()> {
    record_thread_name("standard").await?;
    service_daemon::done();

    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }

    Ok(())
}

#[service(tags = ["__test_scheduling_threads__"], scheduling = HighPriority)]
async fn high_priority_service() -> anyhow::Result<()> {
    record_thread_name("high_priority").await?;
    service_daemon::done();

    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }

    Ok(())
}

#[service(tags = ["__test_scheduling_lifecycle__"], scheduling = Standard)]
async fn standard_lifecycle_service() -> anyhow::Result<()> {
    STANDARD_STARTED.store(true, Ordering::SeqCst);
    service_daemon::done();

    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }

    STANDARD_STOPPED.store(true, Ordering::SeqCst);
    Ok(())
}

#[service(tags = ["__test_scheduling_lifecycle__"], scheduling = HighPriority)]
async fn high_priority_lifecycle_service() -> anyhow::Result<()> {
    HIGH_PRIORITY_STARTED.store(true, Ordering::SeqCst);
    service_daemon::done();

    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }

    HIGH_PRIORITY_STOPPED.store(true, Ordering::SeqCst);
    Ok(())
}

#[service(tags = ["__test_scheduling_lifecycle__"], scheduling = Isolated)]
async fn isolated_lifecycle_service() -> anyhow::Result<()> {
    ISOLATED_STARTED.store(true, Ordering::SeqCst);
    service_daemon::done();

    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }

    ISOLATED_STOPPED.store(true, Ordering::SeqCst);
    Ok(())
}

#[tokio::test]
async fn test_scheduling_isolation() -> anyhow::Result<()> {
    THREAD_NAMES.lock().await.clear();

    let mut daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__test_scheduling_threads__")
                .build(),
        )
        .build();

    let cancel = daemon.cancel_token();
    daemon.run().await;

    tokio::time::sleep(Duration::from_millis(200)).await;
    cancel.cancel();
    daemon.wait().await?;

    let names_guard = THREAD_NAMES.lock().await;
    let names: &HashSet<String> = &*names_guard;

    assert!(
        names.iter().any(|n| n == "isolated:svc-isolated_service"),
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
        !names
            .iter()
            .any(|n| n.starts_with("high_priority:standard:")),
        "HighPriority service should not collapse to Standard thread naming, found: {:?}",
        names
    );

    Ok(())
}

#[tokio::test]
async fn test_scheduling_variants_participate_in_startup_and_shutdown() -> anyhow::Result<()> {
    STANDARD_STARTED.store(false, Ordering::SeqCst);
    STANDARD_STOPPED.store(false, Ordering::SeqCst);
    HIGH_PRIORITY_STARTED.store(false, Ordering::SeqCst);
    HIGH_PRIORITY_STOPPED.store(false, Ordering::SeqCst);
    ISOLATED_STARTED.store(false, Ordering::SeqCst);
    ISOLATED_STOPPED.store(false, Ordering::SeqCst);

    let mut daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__test_scheduling_lifecycle__")
                .build(),
        )
        .build();

    let cancel = daemon.cancel_token();
    daemon.run().await;

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(STANDARD_STARTED.load(Ordering::SeqCst));
    assert!(HIGH_PRIORITY_STARTED.load(Ordering::SeqCst));
    assert!(ISOLATED_STARTED.load(Ordering::SeqCst));

    cancel.cancel();
    daemon.wait().await?;

    assert!(STANDARD_STOPPED.load(Ordering::SeqCst));
    assert!(HIGH_PRIORITY_STOPPED.load(Ordering::SeqCst));
    assert!(ISOLATED_STOPPED.load(Ordering::SeqCst));

    Ok(())
}
