use service_daemon::{
    DiagnosticRestartDecisionKind, Registry, RestartPolicy, ServiceDaemon, ServiceStatus, provider,
    service,
};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;

static RELOAD_EVENTS: LazyLock<Mutex<Vec<&'static str>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static HIGH_PRIORITY_RELOAD_EVENTS: LazyLock<Mutex<Vec<&'static str>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
static ISOLATED_RELOAD_EVENTS: LazyLock<Mutex<Vec<&'static str>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
static RELOAD_GENERATIONS: AtomicU32 = AtomicU32::new(0);
static HIGH_PRIORITY_RELOAD_GENERATIONS: AtomicU32 = AtomicU32::new(0);
static ISOLATED_RELOAD_GENERATIONS: AtomicU32 = AtomicU32::new(0);
static NOOP_RELOAD_EVENTS: LazyLock<Mutex<Vec<&'static str>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
static NOOP_RELOAD_GENERATIONS: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Default)]
#[provider]
pub struct ReloadConfig {
    pub version: u32,
}

#[derive(Clone, Default)]
#[provider]
pub struct HighPriorityReloadConfig {
    pub version: u32,
}

#[derive(Clone, Default)]
#[provider]
pub struct IsolatedReloadConfig {
    pub version: u32,
}

#[derive(Clone, Default)]
#[provider]
pub struct NoopReloadConfig {
    pub version: u32,
}

#[service(tags = ["__test_reload_contract__"])]
async fn reload_observer(_config: Arc<ReloadConfig>) -> anyhow::Result<()> {
    RELOAD_GENERATIONS.fetch_add(1, Ordering::SeqCst);

    loop {
        match service_daemon::state() {
            ServiceStatus::Initializing => {
                RELOAD_EVENTS.lock().await.push("initializing");
                service_daemon::done();
            }
            ServiceStatus::Restoring => {
                RELOAD_EVENTS.lock().await.push("restoring");
                service_daemon::done();
            }
            ServiceStatus::Healthy => {
                if !service_daemon::sleep(Duration::from_millis(20)).await {
                    continue;
                }
            }
            ServiceStatus::NeedReload => {
                RELOAD_EVENTS.lock().await.push("need_reload");
                service_daemon::done();
                break;
            }
            ServiceStatus::ShuttingDown => break,
            _ => break,
        }
    }

    Ok(())
}

#[service(tags = ["__test_high_priority_reload_contract__"], scheduling = HighPriority)]
async fn high_priority_reload_observer(
    _config: Arc<HighPriorityReloadConfig>,
) -> anyhow::Result<()> {
    HIGH_PRIORITY_RELOAD_GENERATIONS.fetch_add(1, Ordering::SeqCst);

    loop {
        match service_daemon::state() {
            ServiceStatus::Initializing => {
                HIGH_PRIORITY_RELOAD_EVENTS
                    .lock()
                    .await
                    .push("initializing");
                service_daemon::done();
            }
            ServiceStatus::Restoring => {
                HIGH_PRIORITY_RELOAD_EVENTS.lock().await.push("restoring");
                service_daemon::done();
            }
            ServiceStatus::Healthy => {
                if !service_daemon::sleep(Duration::from_millis(20)).await {
                    continue;
                }
            }
            ServiceStatus::NeedReload => {
                HIGH_PRIORITY_RELOAD_EVENTS.lock().await.push("need_reload");
                service_daemon::done();
                break;
            }
            ServiceStatus::ShuttingDown => break,
            _ => break,
        }
    }

    Ok(())
}

#[service(tags = ["__test_isolated_reload_contract__"], scheduling = Isolated)]
async fn isolated_reload_observer(_config: Arc<IsolatedReloadConfig>) -> anyhow::Result<()> {
    ISOLATED_RELOAD_GENERATIONS.fetch_add(1, Ordering::SeqCst);

    loop {
        match service_daemon::state() {
            ServiceStatus::Initializing => {
                ISOLATED_RELOAD_EVENTS.lock().await.push("initializing");
                service_daemon::done();
            }
            ServiceStatus::Restoring => {
                ISOLATED_RELOAD_EVENTS.lock().await.push("restoring");
                service_daemon::done();
            }
            ServiceStatus::Healthy => {
                if !service_daemon::sleep(Duration::from_millis(20)).await {
                    continue;
                }
            }
            ServiceStatus::NeedReload => {
                ISOLATED_RELOAD_EVENTS.lock().await.push("need_reload");
                service_daemon::done();
                break;
            }
            ServiceStatus::ShuttingDown => break,
            _ => break,
        }
    }

    Ok(())
}

#[service(tags = ["__test_noop_reload_contract__"])]
async fn noop_reload_observer(_config: Arc<NoopReloadConfig>) -> anyhow::Result<()> {
    NOOP_RELOAD_GENERATIONS.fetch_add(1, Ordering::SeqCst);

    loop {
        match service_daemon::state() {
            ServiceStatus::Initializing => {
                NOOP_RELOAD_EVENTS.lock().await.push("initializing");
                service_daemon::done();
            }
            ServiceStatus::Restoring => {
                NOOP_RELOAD_EVENTS.lock().await.push("restoring");
                service_daemon::done();
            }
            ServiceStatus::Healthy => {
                if !service_daemon::sleep(Duration::from_millis(20)).await {
                    continue;
                }
            }
            ServiceStatus::NeedReload => {
                NOOP_RELOAD_EVENTS.lock().await.push("need_reload");
                service_daemon::done();
                break;
            }
            ServiceStatus::ShuttingDown => break,
            _ => break,
        }
    }

    Ok(())
}

async fn wait_for_event(
    events: &Mutex<Vec<&'static str>>,
    event: &'static str,
    description: &'static str,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if events.lock().await.contains(&event) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect(description);
}

async fn wait_for_generation(counter: &AtomicU32, minimum: u32, description: &'static str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while counter.load(Ordering::SeqCst) < minimum {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect(description);
}

async fn assert_event_absent_for(
    events: &Mutex<Vec<&'static str>>,
    event: &'static str,
    duration: Duration,
    description: &'static str,
) {
    tokio::time::sleep(duration).await;
    let snapshot = events.lock().await.clone();
    assert!(!snapshot.contains(&event), "{description}: {snapshot:?}");
}

#[tokio::test]
async fn test_dependency_reload_transitions_through_need_reload_and_restoring() -> anyhow::Result<()>
{
    RELOAD_GENERATIONS.store(0, Ordering::SeqCst);
    RELOAD_EVENTS.lock().await.clear();

    let daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__test_reload_contract__")
                .build(),
        )
        .with_restart_policy(RestartPolicy::for_testing())
        .build();

    let cancel = daemon.cancel_token();
    daemon.run().await;

    wait_for_event(
        &RELOAD_EVENTS,
        "initializing",
        "initial generation should initialize before mutation",
    )
    .await;

    {
        let lock = <ReloadConfig as service_daemon::ManagedProvided>::resolve_rwlock()
            .await
            .expect("reload config should resolve");
        let mut guard = lock.write().await;
        guard.version += 1;
    }

    wait_for_event(
        &RELOAD_EVENTS,
        "need_reload",
        "dependency mutation should emit NeedReload",
    )
    .await;
    wait_for_event(
        &RELOAD_EVENTS,
        "restoring",
        "dependency mutation should restart a restoring generation",
    )
    .await;
    wait_for_generation(
        &RELOAD_GENERATIONS,
        2,
        "dependency mutation should start a second generation",
    )
    .await;
    cancel.cancel();
    daemon.wait().await?;

    let events = RELOAD_EVENTS.lock().await.clone();
    assert!(
        events.contains(&"initializing"),
        "initial generation not observed: {:?}",
        events
    );
    assert!(
        events.contains(&"need_reload"),
        "NeedReload was not observed after dependency mutation: {:?}",
        events
    );
    assert!(
        events.contains(&"restoring"),
        "restoring generation not observed after reload: {:?}",
        events
    );
    assert!(
        RELOAD_GENERATIONS.load(Ordering::SeqCst) >= 2,
        "expected at least two generations after reload"
    );

    let service = daemon
        .diagnostics_snapshot()
        .services
        .into_iter()
        .find(|service| service.service_name == "reload_observer")
        .expect("reload observer diagnostics should be recorded");
    assert_eq!(service.aggregate.lifecycle.backoff_restart, 0);
    assert_eq!(
        service.aggregate.lifecycle.last_restart_decision,
        Some(DiagnosticRestartDecisionKind::Immediate)
    );

    Ok(())
}

#[tokio::test]
async fn test_high_priority_dependency_reload_crosses_body_lane() -> anyhow::Result<()> {
    HIGH_PRIORITY_RELOAD_GENERATIONS.store(0, Ordering::SeqCst);
    HIGH_PRIORITY_RELOAD_EVENTS.lock().await.clear();

    let daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__test_high_priority_reload_contract__")
                .build(),
        )
        .with_restart_policy(RestartPolicy::for_testing())
        .build();

    let cancel = daemon.cancel_token();
    daemon.run().await;

    wait_for_event(
        &HIGH_PRIORITY_RELOAD_EVENTS,
        "initializing",
        "initial high-priority generation should initialize before mutation",
    )
    .await;

    {
        let lock = <HighPriorityReloadConfig as service_daemon::ManagedProvided>::resolve_rwlock()
            .await
            .expect("high-priority reload config should resolve");
        let mut guard = lock.write().await;
        guard.version += 1;
    }

    wait_for_event(
        &HIGH_PRIORITY_RELOAD_EVENTS,
        "need_reload",
        "high-priority mutation should emit NeedReload",
    )
    .await;
    wait_for_event(
        &HIGH_PRIORITY_RELOAD_EVENTS,
        "restoring",
        "high-priority mutation should restart a restoring generation",
    )
    .await;
    wait_for_generation(
        &HIGH_PRIORITY_RELOAD_GENERATIONS,
        2,
        "high-priority mutation should start a second generation",
    )
    .await;
    cancel.cancel();
    daemon.wait().await?;

    let events = HIGH_PRIORITY_RELOAD_EVENTS.lock().await.clone();
    assert!(
        events.contains(&"initializing"),
        "initial high-priority generation not observed: {:?}",
        events
    );
    assert!(
        events.contains(&"need_reload"),
        "NeedReload was not observed after high-priority dependency mutation: {:?}",
        events
    );
    assert!(
        events.contains(&"restoring"),
        "restoring high-priority generation not observed after reload: {:?}",
        events
    );
    assert!(
        HIGH_PRIORITY_RELOAD_GENERATIONS.load(Ordering::SeqCst) >= 2,
        "expected at least two high-priority generations after reload"
    );

    Ok(())
}

#[tokio::test]
async fn test_isolated_dependency_reload_crosses_generation_bridge() -> anyhow::Result<()> {
    ISOLATED_RELOAD_GENERATIONS.store(0, Ordering::SeqCst);
    ISOLATED_RELOAD_EVENTS.lock().await.clear();

    let daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__test_isolated_reload_contract__")
                .build(),
        )
        .with_restart_policy(RestartPolicy::for_testing())
        .build();

    let cancel = daemon.cancel_token();
    daemon.run().await;

    wait_for_event(
        &ISOLATED_RELOAD_EVENTS,
        "initializing",
        "initial isolated generation should initialize before mutation",
    )
    .await;

    {
        let lock = <IsolatedReloadConfig as service_daemon::ManagedProvided>::resolve_rwlock()
            .await
            .expect("isolated reload config should resolve");
        let mut guard = lock.write().await;
        guard.version += 1;
    }

    wait_for_event(
        &ISOLATED_RELOAD_EVENTS,
        "need_reload",
        "isolated mutation should emit NeedReload",
    )
    .await;
    wait_for_event(
        &ISOLATED_RELOAD_EVENTS,
        "restoring",
        "isolated mutation should restart a restoring generation",
    )
    .await;
    wait_for_generation(
        &ISOLATED_RELOAD_GENERATIONS,
        2,
        "isolated mutation should start a second generation",
    )
    .await;
    cancel.cancel();
    daemon.wait().await?;

    let events = ISOLATED_RELOAD_EVENTS.lock().await.clone();
    assert!(
        events.contains(&"initializing"),
        "initial isolated generation not observed: {:?}",
        events
    );
    assert!(
        events.contains(&"need_reload"),
        "NeedReload was not observed after isolated dependency mutation: {:?}",
        events
    );
    assert!(
        events.contains(&"restoring"),
        "restoring isolated generation not observed after reload: {:?}",
        events
    );
    assert!(
        ISOLATED_RELOAD_GENERATIONS.load(Ordering::SeqCst) >= 2,
        "expected at least two isolated generations after reload"
    );

    Ok(())
}

#[tokio::test]
async fn write_lock_without_mutation_does_not_trigger_reload() -> anyhow::Result<()> {
    NOOP_RELOAD_GENERATIONS.store(0, Ordering::SeqCst);
    NOOP_RELOAD_EVENTS.lock().await.clear();

    let daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__test_noop_reload_contract__")
                .build(),
        )
        .with_restart_policy(RestartPolicy::for_testing())
        .build();

    let cancel = daemon.cancel_token();
    daemon.run().await;

    wait_for_event(
        &NOOP_RELOAD_EVENTS,
        "initializing",
        "noop generation should initialize before write-lock check",
    )
    .await;

    {
        let lock = <NoopReloadConfig as service_daemon::ManagedProvided>::resolve_rwlock()
            .await
            .expect("noop reload config should resolve");
        let guard = lock.write().await;
        let _version = guard.version;
    }

    assert_event_absent_for(
        &NOOP_RELOAD_EVENTS,
        "need_reload",
        Duration::from_millis(400),
        "no-op write lock should not emit NeedReload",
    )
    .await;
    cancel.cancel();
    daemon.wait().await?;

    let events = NOOP_RELOAD_EVENTS.lock().await.clone();
    assert!(
        events.contains(&"initializing"),
        "initial noop generation not observed: {:?}",
        events
    );
    assert!(
        !events.contains(&"need_reload"),
        "NeedReload should not be observed after a no-op write lock: {:?}",
        events
    );
    assert!(
        !events.contains(&"restoring"),
        "restoring should not be observed after a no-op write lock: {:?}",
        events
    );
    assert_eq!(
        NOOP_RELOAD_GENERATIONS.load(Ordering::SeqCst),
        1,
        "no-op write lock should not start a second generation"
    );

    Ok(())
}
