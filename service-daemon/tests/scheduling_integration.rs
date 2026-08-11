use service_daemon::{
    Registry, RestartPolicy, SchedulingAdvisoryProfile, ServiceDaemon, TT::*, provider, service,
    trigger,
};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tokio::sync::Mutex;

static THREAD_NAMES: LazyLock<Arc<Mutex<HashSet<String>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(HashSet::new())));
static ISOLATED_RESTART_THREAD_NAMES: LazyLock<Arc<Mutex<HashSet<String>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(HashSet::new())));
static ISOLATED_PANIC_THREAD_NAMES: LazyLock<Arc<Mutex<HashSet<String>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(HashSet::new())));
static ADVISORY_DISABLED_THREAD_NAMES: LazyLock<Arc<Mutex<HashSet<String>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(HashSet::new())));
static TRIGGER_THREAD_NAMES: LazyLock<Arc<Mutex<HashSet<String>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(HashSet::new())));
static MULTI_ISOLATED_THREAD_NAMES: LazyLock<Arc<Mutex<HashSet<String>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(HashSet::new())));
static STANDARD_STARTED: AtomicBool = AtomicBool::new(false);
static STANDARD_STOPPED: AtomicBool = AtomicBool::new(false);
static HIGH_PRIORITY_STARTED: AtomicBool = AtomicBool::new(false);
static HIGH_PRIORITY_STOPPED: AtomicBool = AtomicBool::new(false);
static ISOLATED_STARTED: AtomicBool = AtomicBool::new(false);
static ISOLATED_STOPPED: AtomicBool = AtomicBool::new(false);
static ISOLATED_RESTART_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
static ISOLATED_RESTART_STOPPED: AtomicBool = AtomicBool::new(false);
static ISOLATED_PANIC_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
static ISOLATED_PANIC_STOPPED: AtomicBool = AtomicBool::new(false);
static ADVISORY_DISABLED_STARTED: AtomicBool = AtomicBool::new(false);
static ADVISORY_DISABLED_STOPPED: AtomicBool = AtomicBool::new(false);
static TRIGGER_DISPATCH_COUNT: AtomicUsize = AtomicUsize::new(0);
static MULTI_ISOLATED_STARTED: AtomicUsize = AtomicUsize::new(0);

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

async fn record_isolated_restart_thread_name() -> anyhow::Result<()> {
    let thread_name = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_string();
    ISOLATED_RESTART_THREAD_NAMES
        .lock()
        .await
        .insert(thread_name);
    Ok(())
}

async fn record_isolated_panic_thread_name() -> anyhow::Result<()> {
    let thread_name = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_string();
    ISOLATED_PANIC_THREAD_NAMES.lock().await.insert(thread_name);
    Ok(())
}

async fn record_advisory_disabled_thread_name() -> anyhow::Result<()> {
    let thread_name = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_string();
    ADVISORY_DISABLED_THREAD_NAMES
        .lock()
        .await
        .insert(thread_name);
    Ok(())
}

async fn record_trigger_thread_name(prefix: &str) -> anyhow::Result<()> {
    let thread_name = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_string();
    TRIGGER_THREAD_NAMES
        .lock()
        .await
        .insert(format!("{}:{}", prefix, thread_name));
    TRIGGER_DISPATCH_COUNT.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

async fn record_multi_isolated_thread_name(prefix: &str) -> anyhow::Result<()> {
    let thread_name = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_string();
    MULTI_ISOLATED_THREAD_NAMES
        .lock()
        .await
        .insert(format!("{}:{}", prefix, thread_name));
    MULTI_ISOLATED_STARTED.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

#[provider(Notify)]
pub struct SchedulingSignal;

#[trigger(
    Event(SchedulingSignal),
    tags = ["__test_trigger_scheduling_lanes__"],
    scheduling = Standard
)]
async fn standard_scheduled_trigger() -> anyhow::Result<()> {
    record_trigger_thread_name("standard_trigger").await
}

#[trigger(
    Event(SchedulingSignal),
    tags = ["__test_trigger_scheduling_lanes__"],
    scheduling = HighPriority
)]
async fn high_priority_scheduled_trigger() -> anyhow::Result<()> {
    record_trigger_thread_name("high_priority_trigger").await
}

#[trigger(
    Event(SchedulingSignal),
    tags = ["__test_trigger_scheduling_lanes__"],
    scheduling = Isolated
)]
async fn isolated_scheduled_trigger() -> anyhow::Result<()> {
    record_trigger_thread_name("isolated_trigger").await
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

#[service(tags = ["__test_multiple_isolated_services__"], scheduling = Isolated)]
async fn multi_isolated_alpha_service() -> anyhow::Result<()> {
    record_multi_isolated_thread_name("alpha").await?;
    service_daemon::done();

    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }

    Ok(())
}

#[service(tags = ["__test_multiple_isolated_services__"], scheduling = Isolated)]
async fn multi_isolated_beta_service() -> anyhow::Result<()> {
    record_multi_isolated_thread_name("beta").await?;
    service_daemon::done();

    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }

    Ok(())
}

#[service(tags = ["__test_multiple_isolated_services__"], scheduling = Isolated)]
async fn multi_isolated_gamma_service() -> anyhow::Result<()> {
    record_multi_isolated_thread_name("gamma").await?;
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

#[service(tags = ["__test_isolated_bridge_restart__"], scheduling = Isolated)]
async fn isolated_restart_service() -> anyhow::Result<()> {
    record_isolated_restart_thread_name().await?;

    if ISOLATED_RESTART_ATTEMPTS.fetch_add(1, Ordering::SeqCst) == 0 {
        return Err(anyhow::anyhow!("simulated isolated generation failure"));
    }

    service_daemon::done();

    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }

    ISOLATED_RESTART_STOPPED.store(true, Ordering::SeqCst);
    Ok(())
}

#[service(tags = ["__test_isolated_bridge_panic__"], scheduling = Isolated)]
async fn isolated_panic_service() -> anyhow::Result<()> {
    record_isolated_panic_thread_name().await?;

    if ISOLATED_PANIC_ATTEMPTS.fetch_add(1, Ordering::SeqCst) == 0 {
        panic!("simulated isolated generation panic");
    }

    service_daemon::done();

    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }

    ISOLATED_PANIC_STOPPED.store(true, Ordering::SeqCst);
    Ok(())
}

#[service(
    tags = ["__test_scheduling_advisory_profile_disabled__"],
    scheduling = HighPriority
)]
async fn advisory_disabled_high_priority_service() -> anyhow::Result<()> {
    ADVISORY_DISABLED_STARTED.store(true, Ordering::SeqCst);
    record_advisory_disabled_thread_name().await?;
    service_daemon::done();

    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }

    ADVISORY_DISABLED_STOPPED.store(true, Ordering::SeqCst);
    Ok(())
}

#[tokio::test]
async fn test_scheduling_isolation() -> anyhow::Result<()> {
    THREAD_NAMES.lock().await.clear();

    let daemon = ServiceDaemon::builder()
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
    let names: &HashSet<String> = &names_guard;

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
        !names.iter().any(|n| n.starts_with("standard:svc-control")),
        "Standard service body should not execute on the control runtime, found: {:?}",
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
        !names
            .iter()
            .any(|n| n.starts_with("high_priority:svc-control")),
        "HighPriority service body should not execute on the control runtime, found: {:?}",
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
async fn test_multiple_isolated_services_get_distinct_private_runtimes() -> anyhow::Result<()> {
    MULTI_ISOLATED_STARTED.store(0, Ordering::SeqCst);
    MULTI_ISOLATED_THREAD_NAMES.lock().await.clear();

    let daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__test_multiple_isolated_services__")
                .build(),
        )
        .build();

    let cancel = daemon.cancel_token();
    daemon.run().await;

    let startup_result = tokio::time::timeout(Duration::from_secs(2), async {
        while MULTI_ISOLATED_STARTED.load(Ordering::SeqCst) < 3 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    cancel.cancel();
    daemon.wait().await?;
    startup_result?;

    let names_guard = MULTI_ISOLATED_THREAD_NAMES.lock().await;
    let names: &HashSet<String> = &names_guard;

    assert_eq!(
        names.len(),
        3,
        "expected exactly three isolated service executions, found: {:?}",
        names
    );
    assert!(
        names.contains("alpha:svc-multi_isolated_alpha_service"),
        "alpha isolated service did not execute on its private runtime, found: {:?}",
        names
    );
    assert!(
        names.contains("beta:svc-multi_isolated_beta_service"),
        "beta isolated service did not execute on its private runtime, found: {:?}",
        names
    );
    assert!(
        names.contains("gamma:svc-multi_isolated_gamma_service"),
        "gamma isolated service did not execute on its private runtime, found: {:?}",
        names
    );
    assert!(
        !names
            .iter()
            .any(|n| n.contains("svc-control") || n.contains("svc-high-priority")),
        "isolated services should not execute on shared daemon runtimes, found: {:?}",
        names
    );

    let thread_names: HashSet<&str> = names
        .iter()
        .filter_map(|entry| entry.split_once(':').map(|(_, thread_name)| thread_name))
        .collect();
    assert_eq!(
        thread_names.len(),
        3,
        "isolated services should use distinct private runtime threads, found: {:?}",
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

    let daemon = ServiceDaemon::builder()
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

#[tokio::test]
async fn test_trigger_scheduling_variants_execute_on_declared_lanes() -> anyhow::Result<()> {
    TRIGGER_DISPATCH_COUNT.store(0, Ordering::SeqCst);
    TRIGGER_THREAD_NAMES.lock().await.clear();

    let daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__test_trigger_scheduling_lanes__")
                .build(),
        )
        .build();

    let cancel = daemon.cancel_token();
    daemon.run().await;

    tokio::time::sleep(Duration::from_millis(200)).await;
    SchedulingSignal::resolve().await.notify();

    let dispatch_result = tokio::time::timeout(Duration::from_secs(2), async {
        while TRIGGER_DISPATCH_COUNT.load(Ordering::SeqCst) < 3 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    cancel.cancel();
    daemon.wait().await?;
    dispatch_result?;

    let names_guard = TRIGGER_THREAD_NAMES.lock().await;
    let names: &HashSet<String> = &names_guard;

    assert!(
        names.iter().any(|n| n.starts_with("standard_trigger:")),
        "Standard trigger did not execute, found: {:?}",
        names
    );
    assert!(
        !names
            .iter()
            .any(|n| n.starts_with("standard_trigger:svc-control")),
        "Standard trigger body should not execute on the control runtime, found: {:?}",
        names
    );
    assert!(
        !names
            .iter()
            .any(|n| n.starts_with("standard_trigger:svc-high-priority")),
        "Standard trigger body should not execute on the HighPriority runtime, found: {:?}",
        names
    );
    assert!(
        !names.contains("standard_trigger:svc-isolated_scheduled_trigger"),
        "Standard trigger body should not execute on the isolated trigger runtime, found: {:?}",
        names
    );

    assert!(
        names
            .iter()
            .any(|n| n.starts_with("high_priority_trigger:svc-high-priority")),
        "HighPriority trigger did not execute on the high-priority runtime, found: {:?}",
        names
    );
    assert!(
        !names
            .iter()
            .any(|n| n.starts_with("high_priority_trigger:svc-control")),
        "HighPriority trigger body should not execute on the control runtime, found: {:?}",
        names
    );
    assert!(
        !names.contains("high_priority_trigger:svc-isolated_scheduled_trigger"),
        "HighPriority trigger body should not execute on the isolated trigger runtime, found: {:?}",
        names
    );

    assert!(
        names.contains("isolated_trigger:svc-isolated_scheduled_trigger"),
        "Isolated trigger did not execute on its private runtime, found: {:?}",
        names
    );
    assert!(
        !names
            .iter()
            .any(|n| n.starts_with("isolated_trigger:svc-control")),
        "Isolated trigger body should not execute on the control runtime, found: {:?}",
        names
    );
    assert!(
        !names
            .iter()
            .any(|n| n.starts_with("isolated_trigger:svc-high-priority")),
        "Isolated trigger body should not execute on the HighPriority runtime, found: {:?}",
        names
    );

    Ok(())
}

#[tokio::test]
async fn test_scheduling_advisory_disable_preserves_lifecycle_and_lane() -> anyhow::Result<()> {
    ADVISORY_DISABLED_STARTED.store(false, Ordering::SeqCst);
    ADVISORY_DISABLED_STOPPED.store(false, Ordering::SeqCst);
    ADVISORY_DISABLED_THREAD_NAMES.lock().await.clear();

    let daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__test_scheduling_advisory_profile_disabled__")
                .build(),
        )
        .with_scheduling_advisory_profile(SchedulingAdvisoryProfile::disabled())
        .build();

    let cancel = daemon.cancel_token();
    daemon.run().await;

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(ADVISORY_DISABLED_STARTED.load(Ordering::SeqCst));
    assert!(
        ADVISORY_DISABLED_THREAD_NAMES
            .lock()
            .await
            .iter()
            .any(|name| name.starts_with("svc-high-priority"))
    );

    cancel.cancel();
    daemon.wait().await?;

    assert!(ADVISORY_DISABLED_STOPPED.load(Ordering::SeqCst));

    Ok(())
}

#[tokio::test]
async fn test_isolated_generation_failure_restarts_through_bridge() -> anyhow::Result<()> {
    ISOLATED_RESTART_ATTEMPTS.store(0, Ordering::SeqCst);
    ISOLATED_RESTART_STOPPED.store(false, Ordering::SeqCst);
    ISOLATED_RESTART_THREAD_NAMES.lock().await.clear();

    let daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__test_isolated_bridge_restart__")
                .build(),
        )
        .with_restart_policy(RestartPolicy::for_testing())
        .build();

    let cancel = daemon.cancel_token();
    daemon.run().await;

    tokio::time::timeout(Duration::from_secs(2), async {
        while ISOLATED_RESTART_ATTEMPTS.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;

    assert!(
        ISOLATED_RESTART_THREAD_NAMES
            .lock()
            .await
            .contains("svc-isolated_restart_service")
    );

    cancel.cancel();
    daemon.wait().await?;

    assert!(ISOLATED_RESTART_STOPPED.load(Ordering::SeqCst));

    Ok(())
}

#[tokio::test]
async fn test_isolated_generation_panic_restarts_through_bridge() -> anyhow::Result<()> {
    ISOLATED_PANIC_ATTEMPTS.store(0, Ordering::SeqCst);
    ISOLATED_PANIC_STOPPED.store(false, Ordering::SeqCst);
    ISOLATED_PANIC_THREAD_NAMES.lock().await.clear();

    let daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__test_isolated_bridge_panic__")
                .build(),
        )
        .with_restart_policy(RestartPolicy::for_testing())
        .build();

    let cancel = daemon.cancel_token();
    daemon.run().await;

    tokio::time::timeout(Duration::from_secs(2), async {
        while ISOLATED_PANIC_ATTEMPTS.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;

    assert!(
        ISOLATED_PANIC_THREAD_NAMES
            .lock()
            .await
            .contains("svc-isolated_panic_service")
    );

    cancel.cancel();
    daemon.wait().await?;

    assert!(ISOLATED_PANIC_STOPPED.load(Ordering::SeqCst));

    Ok(())
}
