#[path = "support/provider_context.rs"]
mod provider_context;

use service_daemon::{
    ProviderDependencyChangeReason, ServiceDaemon, WatchableProvided, provider, service,
};
#[cfg(feature = "simulation")]
use service_daemon::{TT::*, trigger};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::time::timeout;

static EXTERNAL_PEER_PROVIDER_INITS: AtomicUsize = AtomicUsize::new(0);
static DAEMON_SHARED_PROVIDER_INITS: AtomicUsize = AtomicUsize::new(0);
static DAEMON_SHARED_OBSERVATIONS: AtomicUsize = AtomicUsize::new(0);
static DAEMON_SHARED_FIRST_VALUE: AtomicUsize = AtomicUsize::new(0);
static DAEMON_SHARED_SECOND_VALUE: AtomicUsize = AtomicUsize::new(0);
static EAGER_SEED_PROVIDER_INITS: AtomicUsize = AtomicUsize::new(0);
static EAGER_SEED_SERVICE_VALUE: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "simulation")]
static SIMULATION_PROVIDER_INITS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_PROVIDER_OBSERVATIONS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_PROVIDER_FIRST_VALUE: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_PROVIDER_SECOND_VALUE: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_PRE_RUN_OVERRIDE_INITS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_PRE_RUN_OVERRIDE_OBSERVATIONS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_PRE_RUN_OVERRIDE_VALUE: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_RUNTIME_OVERRIDE_INITS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_RUNTIME_OVERRIDE_OBSERVATIONS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_RUNTIME_OVERRIDE_FIRST_VALUE: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_RUNTIME_OVERRIDE_SECOND_VALUE: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_OVERRIDE_BOUNDARY_PEER_INITS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_OVERRIDE_BOUNDARY_OBSERVATIONS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_OVERRIDE_BOUNDARY_FIRST_VALUE: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_OVERRIDE_BOUNDARY_SECOND_VALUE: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_LOCAL_VALUE_INITS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_LOCAL_VALUE_LOCAL_OBSERVATIONS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_LOCAL_VALUE_PEER_OBSERVATIONS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_LOCAL_VALUE_FIRST_VALUE: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_LOCAL_VALUE_SECOND_VALUE: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_LOCAL_VALUE_PEER_VALUE: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_LOCAL_VALUE_MUTATIONS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_WATCH_TARGET_INITS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_WATCH_TARGET_OBSERVATIONS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_WATCH_TARGET_FIRST_VALUE: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_WATCH_TARGET_SECOND_VALUE: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_WATCH_EXTRA_TARGET_INITS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_WATCH_EXTRA_DEP_INITS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_WATCH_EXTRA_OBSERVATIONS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_WATCH_EXTRA_FIRST_DEP_VALUE: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_WATCH_EXTRA_SECOND_DEP_VALUE: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone)]
pub struct ExternalPeerProvider(pub usize);

#[provider]
async fn external_peer_provider() -> ExternalPeerProvider {
    ExternalPeerProvider(EXTERNAL_PEER_PROVIDER_INITS.fetch_add(1, Ordering::SeqCst) + 1)
}

#[derive(Clone)]
pub struct DaemonSharedProvider(pub usize);

#[provider]
async fn daemon_shared_provider() -> DaemonSharedProvider {
    DaemonSharedProvider(DAEMON_SHARED_PROVIDER_INITS.fetch_add(1, Ordering::SeqCst) + 1)
}

#[service(tags = ["provider_scope_daemon_static_cache"])]
async fn daemon_static_cache_service(provider: Arc<DaemonSharedProvider>) -> anyhow::Result<()> {
    let observation = DAEMON_SHARED_OBSERVATIONS.fetch_add(1, Ordering::SeqCst);
    if observation == 0 {
        DAEMON_SHARED_FIRST_VALUE.store(provider.0, Ordering::SeqCst);
    } else if observation == 1 {
        DAEMON_SHARED_SECOND_VALUE.store(provider.0, Ordering::SeqCst);
    }

    service_daemon::done();
    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

#[derive(Clone)]
pub struct EagerSeedProvider(pub usize);

#[provider(eager = true)]
async fn eager_seed_provider() -> EagerSeedProvider {
    EagerSeedProvider(EAGER_SEED_PROVIDER_INITS.fetch_add(1, Ordering::SeqCst) + 1)
}

#[service(tags = ["provider_scope_eager_seed_cache"])]
async fn eager_seed_cache_service(provider: Arc<EagerSeedProvider>) -> anyhow::Result<()> {
    assert!(Arc::ptr_eq(&provider, &EagerSeedProvider::resolve().await));
    EAGER_SEED_SERVICE_VALUE.store(provider.0, Ordering::SeqCst);
    service_daemon::done();
    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

#[derive(Clone)]
pub struct WatchCharacterizationProvider(pub usize);

#[provider]
async fn watch_characterization_provider() -> WatchCharacterizationProvider {
    WatchCharacterizationProvider(1)
}

#[cfg(feature = "simulation")]
#[derive(Clone)]
pub struct SimulationProviderCache(pub usize);

#[cfg(feature = "simulation")]
#[provider]
async fn simulation_provider_cache() -> SimulationProviderCache {
    SimulationProviderCache(SIMULATION_PROVIDER_INITS.fetch_add(1, Ordering::SeqCst) + 1)
}

#[cfg(feature = "simulation")]
#[service(tags = ["simulation_provider_cache_shared"])]
async fn simulation_provider_cache_service(
    provider: Arc<SimulationProviderCache>,
) -> anyhow::Result<()> {
    let observation = SIMULATION_PROVIDER_OBSERVATIONS.fetch_add(1, Ordering::SeqCst);
    if observation == 0 {
        SIMULATION_PROVIDER_FIRST_VALUE.store(provider.0, Ordering::SeqCst);
    } else if observation == 1 {
        SIMULATION_PROVIDER_SECOND_VALUE.store(provider.0, Ordering::SeqCst);
    }

    service_daemon::done();
    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

#[cfg(feature = "simulation")]
#[derive(Clone)]
pub struct SimulationPreRunOverrideProvider(pub usize);

#[cfg(feature = "simulation")]
#[provider(eager = true)]
async fn simulation_pre_run_override_provider() -> SimulationPreRunOverrideProvider {
    SimulationPreRunOverrideProvider(
        SIMULATION_PRE_RUN_OVERRIDE_INITS.fetch_add(1, Ordering::SeqCst) + 1,
    )
}

#[cfg(feature = "simulation")]
#[service(tags = ["simulation_pre_run_provider_override"])]
async fn simulation_pre_run_override_service(
    provider: Arc<SimulationPreRunOverrideProvider>,
) -> anyhow::Result<()> {
    SIMULATION_PRE_RUN_OVERRIDE_OBSERVATIONS.fetch_add(1, Ordering::SeqCst);
    SIMULATION_PRE_RUN_OVERRIDE_VALUE.store(provider.0, Ordering::SeqCst);

    service_daemon::done();
    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

#[cfg(feature = "simulation")]
#[derive(Clone)]
pub struct SimulationRuntimeOverrideProvider(pub usize);

#[cfg(feature = "simulation")]
#[provider]
async fn simulation_runtime_override_provider() -> SimulationRuntimeOverrideProvider {
    SimulationRuntimeOverrideProvider(
        SIMULATION_RUNTIME_OVERRIDE_INITS.fetch_add(1, Ordering::SeqCst) + 1,
    )
}

#[cfg(feature = "simulation")]
#[service(tags = ["simulation_runtime_provider_override"])]
async fn simulation_runtime_override_service(
    provider: Arc<SimulationRuntimeOverrideProvider>,
) -> anyhow::Result<()> {
    let observation = SIMULATION_RUNTIME_OVERRIDE_OBSERVATIONS.fetch_add(1, Ordering::SeqCst);
    if observation == 0 {
        SIMULATION_RUNTIME_OVERRIDE_FIRST_VALUE.store(provider.0, Ordering::SeqCst);
    } else if observation == 1 {
        SIMULATION_RUNTIME_OVERRIDE_SECOND_VALUE.store(provider.0, Ordering::SeqCst);
    }

    service_daemon::done();
    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

#[cfg(feature = "simulation")]
#[derive(Clone)]
pub struct SimulationOverrideBoundaryProvider(pub usize);

#[cfg(feature = "simulation")]
#[provider]
async fn simulation_override_boundary_provider() -> SimulationOverrideBoundaryProvider {
    SimulationOverrideBoundaryProvider(
        SIMULATION_OVERRIDE_BOUNDARY_PEER_INITS.fetch_add(1, Ordering::SeqCst) + 1,
    )
}

#[cfg(feature = "simulation")]
#[service(tags = ["simulation_override_boundary"])]
async fn simulation_override_boundary_service(
    provider: Arc<SimulationOverrideBoundaryProvider>,
) -> anyhow::Result<()> {
    let observation = SIMULATION_OVERRIDE_BOUNDARY_OBSERVATIONS.fetch_add(1, Ordering::SeqCst);
    if observation == 0 {
        SIMULATION_OVERRIDE_BOUNDARY_FIRST_VALUE.store(provider.0, Ordering::SeqCst);
    } else if observation == 1 {
        SIMULATION_OVERRIDE_BOUNDARY_SECOND_VALUE.store(provider.0, Ordering::SeqCst);
    }

    service_daemon::done();
    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

#[cfg(feature = "simulation")]
#[derive(Clone)]
pub struct SimulationLocalValueProvider(pub usize);

#[cfg(feature = "simulation")]
#[provider]
async fn simulation_local_value_provider() -> SimulationLocalValueProvider {
    SimulationLocalValueProvider(SIMULATION_LOCAL_VALUE_INITS.fetch_add(1, Ordering::SeqCst) + 1)
}

#[cfg(feature = "simulation")]
#[service(tags = ["simulation_local_value_boundary"])]
async fn simulation_local_value_service(
    provider: Arc<service_daemon::RwLock<SimulationLocalValueProvider>>,
) -> anyhow::Result<()> {
    let value = provider.read().await.0;
    if value >= 40 {
        let observation = SIMULATION_LOCAL_VALUE_LOCAL_OBSERVATIONS.fetch_add(1, Ordering::SeqCst);
        if observation == 0 {
            SIMULATION_LOCAL_VALUE_FIRST_VALUE.store(value, Ordering::SeqCst);
        } else if observation == 1 {
            SIMULATION_LOCAL_VALUE_SECOND_VALUE.store(value, Ordering::SeqCst);
        }
    } else {
        SIMULATION_LOCAL_VALUE_PEER_OBSERVATIONS.fetch_add(1, Ordering::SeqCst);
        SIMULATION_LOCAL_VALUE_PEER_VALUE.store(value, Ordering::SeqCst);
    }

    service_daemon::done();
    if value == 42
        && SIMULATION_LOCAL_VALUE_MUTATIONS
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    {
        service_daemon::sleep(Duration::from_millis(100)).await;
        let mut guard = provider.write().await;
        guard.0 = 43;
    }

    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

#[cfg(feature = "simulation")]
#[derive(Clone)]
pub struct SimulationWatchTargetProvider(pub usize);

#[cfg(feature = "simulation")]
#[provider]
async fn simulation_watch_target_provider() -> SimulationWatchTargetProvider {
    SimulationWatchTargetProvider(SIMULATION_WATCH_TARGET_INITS.fetch_add(1, Ordering::SeqCst) + 1)
}

#[cfg(feature = "simulation")]
#[trigger(Watch(SimulationWatchTargetProvider), tags = ["simulation_watch_target_provider"])]
async fn simulation_watch_target_trigger(
    snapshot: Arc<SimulationWatchTargetProvider>,
) -> anyhow::Result<()> {
    let observation = SIMULATION_WATCH_TARGET_OBSERVATIONS.fetch_add(1, Ordering::SeqCst);
    if observation == 0 {
        SIMULATION_WATCH_TARGET_FIRST_VALUE.store(snapshot.0, Ordering::SeqCst);
    } else if observation == 1 {
        SIMULATION_WATCH_TARGET_SECOND_VALUE.store(snapshot.0, Ordering::SeqCst);
    }
    Ok(())
}

#[cfg(feature = "simulation")]
#[derive(Clone)]
pub struct SimulationWatchExtraTargetProvider(pub usize);

#[cfg(feature = "simulation")]
#[provider]
async fn simulation_watch_extra_target_provider() -> SimulationWatchExtraTargetProvider {
    SimulationWatchExtraTargetProvider(
        SIMULATION_WATCH_EXTRA_TARGET_INITS.fetch_add(1, Ordering::SeqCst) + 1,
    )
}

#[cfg(feature = "simulation")]
#[derive(Clone)]
pub struct SimulationWatchExtraDependencyProvider(pub usize);

#[cfg(feature = "simulation")]
#[provider]
async fn simulation_watch_extra_dependency_provider() -> SimulationWatchExtraDependencyProvider {
    SimulationWatchExtraDependencyProvider(
        SIMULATION_WATCH_EXTRA_DEP_INITS.fetch_add(1, Ordering::SeqCst) + 1,
    )
}

#[cfg(feature = "simulation")]
#[trigger(
    Watch(SimulationWatchExtraTargetProvider),
    tags = ["simulation_watch_extra_dependency"]
)]
async fn simulation_watch_extra_dependency_trigger(
    _snapshot: Arc<SimulationWatchExtraTargetProvider>,
    dependency: Arc<SimulationWatchExtraDependencyProvider>,
) -> anyhow::Result<()> {
    let observation = SIMULATION_WATCH_EXTRA_OBSERVATIONS.fetch_add(1, Ordering::SeqCst);
    if observation == 0 {
        SIMULATION_WATCH_EXTRA_FIRST_DEP_VALUE.store(dependency.0, Ordering::SeqCst);
    } else if observation == 1 {
        SIMULATION_WATCH_EXTRA_SECOND_DEP_VALUE.store(dependency.0, Ordering::SeqCst);
    }
    Ok(())
}

async fn wait_until(description: &'static str, mut condition: impl FnMut() -> bool) {
    timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect(description);
}

async fn wait_for_observations(description: &'static str, observed: &AtomicUsize, count: usize) {
    wait_until(description, || observed.load(Ordering::SeqCst) >= count).await;
}

#[cfg(feature = "simulation")]
async fn assert_observations_hold(
    description: &'static str,
    observed: &AtomicUsize,
    expected: usize,
    duration: Duration,
) {
    tokio::time::sleep(duration).await;
    assert_eq!(observed.load(Ordering::SeqCst), expected, "{description}");
}

async fn run_tagged_daemon_until_observed(tag: &'static str, observed: &AtomicUsize, count: usize) {
    let daemon = ServiceDaemon::builder()
        .with_registry(service_daemon::Registry::builder().with_tag(tag).build())
        .build();
    let cancel = daemon.cancel_token();

    daemon.run().await;
    wait_for_observations(
        "tagged daemon service should report observation before timeout",
        observed,
        count,
    )
    .await;

    cancel.cancel();
    timeout(Duration::from_secs(5), daemon.wait())
        .await
        .expect("daemon wait should not time out")
        .expect("daemon wait should succeed");
}

#[tokio::test]
async fn daemon_provider_resolutions_share_one_local_cache() -> anyhow::Result<()> {
    provider_context::run(async move {
        let first = ExternalPeerProvider::resolve().await;
        let second = ExternalPeerProvider::resolve().await;

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.0, 1);
        assert_eq!(second.0, 1);
        assert_eq!(EXTERNAL_PEER_PROVIDER_INITS.load(Ordering::SeqCst), 1);
        Ok(())
    })
    .await
}

#[tokio::test]
async fn separate_daemons_isolate_provider_cache_by_default() {
    run_tagged_daemon_until_observed(
        "provider_scope_daemon_static_cache",
        &DAEMON_SHARED_OBSERVATIONS,
        1,
    )
    .await;
    run_tagged_daemon_until_observed(
        "provider_scope_daemon_static_cache",
        &DAEMON_SHARED_OBSERVATIONS,
        2,
    )
    .await;

    assert_eq!(DAEMON_SHARED_PROVIDER_INITS.load(Ordering::SeqCst), 2);
    assert_eq!(DAEMON_SHARED_FIRST_VALUE.load(Ordering::SeqCst), 1);
    assert_eq!(DAEMON_SHARED_SECOND_VALUE.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn eager_provider_initialization_seeds_the_generated_provider_cache() {
    run_tagged_daemon_until_observed(
        "provider_scope_eager_seed_cache",
        &EAGER_SEED_SERVICE_VALUE,
        1,
    )
    .await;

    assert_eq!(EAGER_SEED_PROVIDER_INITS.load(Ordering::SeqCst), 1);
    assert_eq!(EAGER_SEED_SERVICE_VALUE.load(Ordering::SeqCst), 1);
    run_tagged_daemon_until_observed(
        "provider_scope_eager_seed_cache",
        &EAGER_SEED_SERVICE_VALUE,
        2,
    )
    .await;
    assert_eq!(EAGER_SEED_PROVIDER_INITS.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn watchable_provider_dependency_watch_tracks_daemon_slot() -> anyhow::Result<()> {
    provider_context::run(async move {
        let lock = WatchCharacterizationProvider::resolve_rwlock().await;
        let watch = WatchCharacterizationProvider::watch_dependency();
        let changed = tokio::spawn(async move { watch.changed().await });
        tokio::time::sleep(Duration::from_millis(50)).await;

        {
            let mut guard = lock.write().await;
            guard.0 = 2;
        }

        let change = timeout(Duration::from_secs(5), changed).await??;
        assert_eq!(change.reason, ProviderDependencyChangeReason::Value);
        Ok(())
    })
    .await
}

#[cfg(feature = "simulation")]
#[tokio::test]
async fn simulation_daemons_isolate_provider_cache_by_default() {
    use service_daemon::{MockContext, Registry};

    async fn run_simulation_until_observed(count: usize) {
        let simulation = MockContext::builder()
            .with_logging(false)
            .with_registry(
                Registry::builder()
                    .with_tag("simulation_provider_cache_shared")
                    .build(),
            )
            .build();

        simulation
            .run_for_duration(Duration::from_millis(200))
            .await
            .expect("simulation daemon should run for duration");

        wait_for_observations(
            "simulation service should report observation before timeout",
            &SIMULATION_PROVIDER_OBSERVATIONS,
            count,
        )
        .await;
    }

    run_simulation_until_observed(1).await;
    run_simulation_until_observed(2).await;

    assert_eq!(SIMULATION_PROVIDER_INITS.load(Ordering::SeqCst), 2);
    assert_eq!(SIMULATION_PROVIDER_FIRST_VALUE.load(Ordering::SeqCst), 1);
    assert_eq!(SIMULATION_PROVIDER_SECOND_VALUE.load(Ordering::SeqCst), 2);
}

#[cfg(feature = "simulation")]
#[tokio::test]
async fn simulation_pre_run_provider_override_is_daemon_local() -> anyhow::Result<()> {
    provider_context::run(async move {
        use service_daemon::{MockContext, Registry};

        SIMULATION_PRE_RUN_OVERRIDE_INITS.store(0, Ordering::SeqCst);
        SIMULATION_PRE_RUN_OVERRIDE_OBSERVATIONS.store(0, Ordering::SeqCst);
        SIMULATION_PRE_RUN_OVERRIDE_VALUE.store(0, Ordering::SeqCst);

        let simulation = MockContext::builder()
            .with_logging(false)
            .with_provider_override(SimulationPreRunOverrideProvider(42))
            .with_registry(
                Registry::builder()
                    .with_tag("simulation_pre_run_provider_override")
                    .build(),
            )
            .build();

        simulation
            .run_for_duration(Duration::from_millis(200))
            .await
            .expect("simulation daemon should run for duration");

        wait_for_observations(
            "pre-run override service should report observation before timeout",
            &SIMULATION_PRE_RUN_OVERRIDE_OBSERVATIONS,
            1,
        )
        .await;

        assert_eq!(SIMULATION_PRE_RUN_OVERRIDE_VALUE.load(Ordering::SeqCst), 42);
        assert_eq!(SIMULATION_PRE_RUN_OVERRIDE_INITS.load(Ordering::SeqCst), 0);

        let peer = SimulationPreRunOverrideProvider::resolve().await;
        assert_eq!(peer.0, 1);
        assert_eq!(SIMULATION_PRE_RUN_OVERRIDE_INITS.load(Ordering::SeqCst), 1);

        let simulation = MockContext::builder()
            .with_logging(false)
            .with_registry(
                Registry::builder()
                    .with_tag("simulation_pre_run_provider_override")
                    .build(),
            )
            .build();
        simulation
            .run_for_duration(Duration::from_millis(200))
            .await
            .expect("second simulation daemon should run for duration");
        wait_for_observations(
            "second simulation daemon should report observation before timeout",
            &SIMULATION_PRE_RUN_OVERRIDE_OBSERVATIONS,
            2,
        )
        .await;
        assert_eq!(SIMULATION_PRE_RUN_OVERRIDE_VALUE.load(Ordering::SeqCst), 2);

        run_tagged_daemon_until_observed(
            "simulation_pre_run_provider_override",
            &SIMULATION_PRE_RUN_OVERRIDE_OBSERVATIONS,
            3,
        )
        .await;
        assert_eq!(SIMULATION_PRE_RUN_OVERRIDE_VALUE.load(Ordering::SeqCst), 3);
        Ok(())
    })
    .await
}

#[cfg(feature = "simulation")]
#[tokio::test]
async fn simulation_runtime_provider_override_reloads_dependent_service() -> anyhow::Result<()> {
    provider_context::run(async move {
        use service_daemon::{MockContext, Registry};

        SIMULATION_RUNTIME_OVERRIDE_INITS.store(0, Ordering::SeqCst);
        SIMULATION_RUNTIME_OVERRIDE_OBSERVATIONS.store(0, Ordering::SeqCst);
        SIMULATION_RUNTIME_OVERRIDE_FIRST_VALUE.store(0, Ordering::SeqCst);
        SIMULATION_RUNTIME_OVERRIDE_SECOND_VALUE.store(0, Ordering::SeqCst);

        let simulation = MockContext::builder()
            .with_logging(false)
            .with_registry(
                Registry::builder()
                    .with_tag("simulation_runtime_provider_override")
                    .build(),
            )
            .build();
        let cancel = simulation.cancel_token();
        let runner = simulation.clone();
        let daemon_task = tokio::spawn(async move {
            runner.run().await;
            runner.wait().await.expect("daemon wait should succeed");
        });

        wait_for_observations(
            "runtime override service should report first observation before timeout",
            &SIMULATION_RUNTIME_OVERRIDE_OBSERVATIONS,
            1,
        )
        .await;
        simulation.override_provider(SimulationRuntimeOverrideProvider(42));

        wait_for_observations(
            "runtime override should trigger reload and second observation",
            &SIMULATION_RUNTIME_OVERRIDE_OBSERVATIONS,
            2,
        )
        .await;

        cancel.cancel();
        daemon_task
            .await
            .expect("daemon task should join after cancellation");

        assert_eq!(
            SIMULATION_RUNTIME_OVERRIDE_FIRST_VALUE.load(Ordering::SeqCst),
            1
        );
        assert_eq!(
            SIMULATION_RUNTIME_OVERRIDE_SECOND_VALUE.load(Ordering::SeqCst),
            42
        );
        assert_eq!(SIMULATION_RUNTIME_OVERRIDE_INITS.load(Ordering::SeqCst), 1);

        let other = SimulationRuntimeOverrideProvider::resolve().await;
        assert_eq!(other.0, 2);
        Ok(())
    })
    .await
}

#[cfg(feature = "simulation")]
#[tokio::test]
async fn other_daemon_value_mutation_does_not_reload_local_override() -> anyhow::Result<()> {
    provider_context::run(async move {
        use service_daemon::{MockContext, Registry};

        SIMULATION_OVERRIDE_BOUNDARY_PEER_INITS.store(0, Ordering::SeqCst);
        SIMULATION_OVERRIDE_BOUNDARY_OBSERVATIONS.store(0, Ordering::SeqCst);
        SIMULATION_OVERRIDE_BOUNDARY_FIRST_VALUE.store(0, Ordering::SeqCst);
        SIMULATION_OVERRIDE_BOUNDARY_SECOND_VALUE.store(0, Ordering::SeqCst);

        let simulation = MockContext::builder()
            .with_logging(false)
            .with_provider_override(SimulationOverrideBoundaryProvider(42))
            .with_registry(
                Registry::builder()
                    .with_tag("simulation_override_boundary")
                    .build(),
            )
            .build();
        let cancel = simulation.cancel_token();
        let runner = simulation.clone();
        let daemon_task = tokio::spawn(async move {
            runner.run().await;
            runner.wait().await.expect("daemon wait should succeed");
        });

        wait_for_observations(
            "local override service should report first observation",
            &SIMULATION_OVERRIDE_BOUNDARY_OBSERVATIONS,
            1,
        )
        .await;

        {
            let lock = SimulationOverrideBoundaryProvider::resolve_rwlock().await;
            let mut guard = lock.write().await;
            guard.0 = 7;
        }

        assert_observations_hold(
            "peer mutation should not reload daemon after local override",
            &SIMULATION_OVERRIDE_BOUNDARY_OBSERVATIONS,
            1,
            Duration::from_millis(300),
        )
        .await;
        cancel.cancel();
        daemon_task
            .await
            .expect("daemon task should join after cancellation");

        assert_eq!(
            SIMULATION_OVERRIDE_BOUNDARY_FIRST_VALUE.load(Ordering::SeqCst),
            42
        );
        assert_eq!(
            SIMULATION_OVERRIDE_BOUNDARY_SECOND_VALUE.load(Ordering::SeqCst),
            0
        );
        assert_eq!(
            SIMULATION_OVERRIDE_BOUNDARY_OBSERVATIONS.load(Ordering::SeqCst),
            1
        );
        Ok(())
    })
    .await
}

#[cfg(feature = "simulation")]
#[tokio::test]
async fn daemon_local_value_mutation_reloads_only_that_daemon() {
    use service_daemon::{MockContext, Registry};

    SIMULATION_LOCAL_VALUE_INITS.store(0, Ordering::SeqCst);
    SIMULATION_LOCAL_VALUE_LOCAL_OBSERVATIONS.store(0, Ordering::SeqCst);
    SIMULATION_LOCAL_VALUE_PEER_OBSERVATIONS.store(0, Ordering::SeqCst);
    SIMULATION_LOCAL_VALUE_FIRST_VALUE.store(0, Ordering::SeqCst);
    SIMULATION_LOCAL_VALUE_SECOND_VALUE.store(0, Ordering::SeqCst);
    SIMULATION_LOCAL_VALUE_PEER_VALUE.store(0, Ordering::SeqCst);
    SIMULATION_LOCAL_VALUE_MUTATIONS.store(0, Ordering::SeqCst);

    let local_simulation = MockContext::builder()
        .with_logging(false)
        .with_provider_override(SimulationLocalValueProvider(42))
        .with_registry(
            Registry::builder()
                .with_tag("simulation_local_value_boundary")
                .build(),
        )
        .build();
    let local_cancel = local_simulation.cancel_token();
    let local_runner = local_simulation.clone();
    let local_task = tokio::spawn(async move {
        local_runner.run().await;
        local_runner
            .wait()
            .await
            .expect("local daemon wait should succeed");
    });

    let peer_simulation = MockContext::builder()
        .with_logging(false)
        .with_registry(
            Registry::builder()
                .with_tag("simulation_local_value_boundary")
                .build(),
        )
        .build();
    let peer_cancel = peer_simulation.cancel_token();
    let peer_runner = peer_simulation.clone();
    let peer_task = tokio::spawn(async move {
        peer_runner.run().await;
        peer_runner
            .wait()
            .await
            .expect("peer daemon wait should succeed");
    });

    wait_until(
        "both local and peer daemons should report first observation",
        || {
            SIMULATION_LOCAL_VALUE_LOCAL_OBSERVATIONS.load(Ordering::SeqCst) >= 1
                && SIMULATION_LOCAL_VALUE_PEER_OBSERVATIONS.load(Ordering::SeqCst) >= 1
        },
    )
    .await;

    wait_for_observations(
        "local value mutation should reload local daemon",
        &SIMULATION_LOCAL_VALUE_LOCAL_OBSERVATIONS,
        2,
    )
    .await;
    assert_observations_hold(
        "local mutation should not reload the peer daemon",
        &SIMULATION_LOCAL_VALUE_PEER_OBSERVATIONS,
        1,
        Duration::from_millis(200),
    )
    .await;

    local_cancel.cancel();
    peer_cancel.cancel();
    local_task
        .await
        .expect("local daemon task should join after cancellation");
    peer_task
        .await
        .expect("peer daemon task should join after cancellation");

    assert_eq!(
        SIMULATION_LOCAL_VALUE_FIRST_VALUE.load(Ordering::SeqCst),
        42
    );
    assert_eq!(
        SIMULATION_LOCAL_VALUE_SECOND_VALUE.load(Ordering::SeqCst),
        43
    );
    assert_eq!(SIMULATION_LOCAL_VALUE_PEER_VALUE.load(Ordering::SeqCst), 1);
    assert_eq!(
        SIMULATION_LOCAL_VALUE_PEER_OBSERVATIONS.load(Ordering::SeqCst),
        1
    );
}

#[cfg(feature = "simulation")]
#[tokio::test]
async fn watch_trigger_target_resolution_uses_daemon_local_override() {
    use service_daemon::{MockContext, Registry};

    SIMULATION_WATCH_TARGET_INITS.store(0, Ordering::SeqCst);
    SIMULATION_WATCH_TARGET_OBSERVATIONS.store(0, Ordering::SeqCst);
    SIMULATION_WATCH_TARGET_FIRST_VALUE.store(0, Ordering::SeqCst);
    SIMULATION_WATCH_TARGET_SECOND_VALUE.store(0, Ordering::SeqCst);

    let simulation = MockContext::builder()
        .with_logging(false)
        .with_registry(
            Registry::builder()
                .with_tag("simulation_watch_target_provider")
                .build(),
        )
        .build();
    let cancel = simulation.cancel_token();
    let runner = simulation.clone();
    let daemon_task = tokio::spawn(async move {
        runner.run().await;
        runner.wait().await.expect("daemon wait should succeed");
    });

    wait_for_observations(
        "watch trigger should report peer snapshot",
        &SIMULATION_WATCH_TARGET_OBSERVATIONS,
        1,
    )
    .await;

    simulation.override_provider(SimulationWatchTargetProvider(42));

    wait_for_observations(
        "watch target override should reload trigger generation",
        &SIMULATION_WATCH_TARGET_OBSERVATIONS,
        2,
    )
    .await;

    cancel.cancel();
    daemon_task
        .await
        .expect("daemon task should join after cancellation");

    assert_eq!(
        SIMULATION_WATCH_TARGET_FIRST_VALUE.load(Ordering::SeqCst),
        1
    );
    assert_eq!(
        SIMULATION_WATCH_TARGET_SECOND_VALUE.load(Ordering::SeqCst),
        42
    );
}

#[cfg(feature = "simulation")]
#[tokio::test]
async fn watch_trigger_extra_dependency_resolution_uses_daemon_local_override() {
    use service_daemon::{MockContext, Registry};

    SIMULATION_WATCH_EXTRA_TARGET_INITS.store(0, Ordering::SeqCst);
    SIMULATION_WATCH_EXTRA_DEP_INITS.store(0, Ordering::SeqCst);
    SIMULATION_WATCH_EXTRA_OBSERVATIONS.store(0, Ordering::SeqCst);
    SIMULATION_WATCH_EXTRA_FIRST_DEP_VALUE.store(0, Ordering::SeqCst);
    SIMULATION_WATCH_EXTRA_SECOND_DEP_VALUE.store(0, Ordering::SeqCst);

    let simulation = MockContext::builder()
        .with_logging(false)
        .with_registry(
            Registry::builder()
                .with_tag("simulation_watch_extra_dependency")
                .build(),
        )
        .build();
    let cancel = simulation.cancel_token();
    let runner = simulation.clone();
    let daemon_task = tokio::spawn(async move {
        runner.run().await;
        runner.wait().await.expect("daemon wait should succeed");
    });

    wait_for_observations(
        "watch trigger should report peer dependency",
        &SIMULATION_WATCH_EXTRA_OBSERVATIONS,
        1,
    )
    .await;

    simulation.override_provider(SimulationWatchExtraDependencyProvider(42));

    wait_for_observations(
        "extra dependency override should reload trigger generation",
        &SIMULATION_WATCH_EXTRA_OBSERVATIONS,
        2,
    )
    .await;

    cancel.cancel();
    daemon_task
        .await
        .expect("daemon task should join after cancellation");

    assert_eq!(
        SIMULATION_WATCH_EXTRA_FIRST_DEP_VALUE.load(Ordering::SeqCst),
        1
    );
    assert_eq!(
        SIMULATION_WATCH_EXTRA_SECOND_DEP_VALUE.load(Ordering::SeqCst),
        42
    );
}
