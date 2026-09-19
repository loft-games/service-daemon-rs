use service_daemon::{
    ProviderError, ProviderInitError, Registry, RestartPolicy, ServiceDaemon, done, provider,
    provider_contract, provider_impl, service, service_handle,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

fn short_policy() -> RestartPolicy {
    RestartPolicy {
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(2),
        multiplier: 1.0,
        reset_after: Duration::from_secs(1),
        jitter_factor: 0.0,
        wave_spawn_timeout: Duration::from_millis(10),
        provider_init_timeout: Duration::from_millis(12),
        wave_stop_timeout: Duration::from_millis(10),
        trigger_max_retries: None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[provider_contract]
struct PriorityContract(&'static str);

#[provider_impl(priority = 10)]
async fn low_priority_contract() -> PriorityContract {
    PriorityContract("low")
}

#[provider_impl(priority = 90)]
async fn high_priority_contract() -> PriorityContract {
    PriorityContract("high")
}

#[tokio::test]
async fn provider_contract_uses_highest_priority_candidate() {
    let value = PriorityContract::resolve()
        .await
        .expect("contract should resolve");

    assert_eq!(value.0, "high");
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[provider_contract]
struct FallbackContract(&'static str);

static FALLBACK_UNAVAILABLE_CALLS: AtomicU32 = AtomicU32::new(0);

#[provider_impl(priority = 90)]
async fn unavailable_fallback_contract() -> Result<FallbackContract, ProviderError> {
    FALLBACK_UNAVAILABLE_CALLS.fetch_add(1, Ordering::SeqCst);
    Err(ProviderError::Unavailable(
        "candidate is unavailable in this deployment".to_owned(),
    ))
}

#[provider_impl(priority = 10)]
async fn available_fallback_contract() -> FallbackContract {
    FallbackContract("fallback")
}

#[tokio::test]
async fn provider_contract_advances_after_unavailable_candidate() {
    let value = FallbackContract::resolve()
        .await
        .expect("fallback contract should resolve");

    assert_eq!(value.0, "fallback");
    assert_eq!(FALLBACK_UNAVAILABLE_CALLS.load(Ordering::SeqCst), 1);
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[provider_contract]
struct TimeoutFallbackContract(&'static str);

static TIMEOUT_CANDIDATE_CALLS: AtomicU32 = AtomicU32::new(0);

#[provider_impl(priority = 90)]
async fn timeout_candidate_contract() -> Result<TimeoutFallbackContract, ProviderError> {
    TIMEOUT_CANDIDATE_CALLS.fetch_add(1, Ordering::SeqCst);
    Err(ProviderError::Retryable(
        "upstream still booting".to_owned(),
    ))
}

#[provider_impl(priority = 10)]
async fn timeout_fallback_contract() -> TimeoutFallbackContract {
    TimeoutFallbackContract("after-timeout")
}

#[tokio::test]
async fn provider_contract_advances_after_candidate_retry_timeout() {
    let value = service_daemon::__private::resolve_provider_contract::<TimeoutFallbackContract>(
        "TimeoutFallbackContract",
        short_policy(),
        CancellationToken::new(),
    )
    .await
    .expect("timeout fallback contract should resolve");

    assert_eq!(value.0, "after-timeout");
    assert!(TIMEOUT_CANDIDATE_CALLS.load(Ordering::SeqCst) > 0);
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[provider_contract]
struct FatalContract(&'static str);

#[provider_impl(priority = 90)]
async fn fatal_candidate_contract() -> Result<FatalContract, ProviderError> {
    Err(ProviderError::Fatal("bad local configuration".to_owned()))
}

#[provider_impl(priority = 10)]
async fn ignored_after_fatal_contract() -> FatalContract {
    FatalContract("must-not-run")
}

#[tokio::test]
async fn provider_contract_stops_after_fatal_candidate() {
    let result = service_daemon::__private::resolve_provider_contract::<FatalContract>(
        "FatalContract",
        short_policy(),
        CancellationToken::new(),
    )
    .await;

    match result {
        Err(failure) => {
            assert_eq!(
                failure.source(),
                service_daemon::__private::ProviderInitSourceKind::UserProviderFatal
            );
            match failure.into_error() {
                ProviderInitError::Fatal { provider, message } => {
                    assert_eq!(provider, "fatal_candidate_contract");
                    assert!(message.contains("bad local configuration"));
                }
                other => panic!("expected fatal provider error, got {other:?}"),
            }
        }
        Ok(_) => panic!("fatal candidate should stop contract resolution"),
    }
}

#[derive(Clone, Debug)]
#[provider_contract]
struct PanicContract;

#[provider_impl]
async fn panic_contract_impl() -> PanicContract {
    panic!("candidate panic fact")
}

#[tokio::test]
async fn provider_contract_preserves_candidate_panic_source() {
    let result = service_daemon::__private::resolve_provider_contract::<PanicContract>(
        "PanicContract",
        short_policy(),
        CancellationToken::new(),
    )
    .await;

    let failure = result.expect_err("panicking candidate should fail");
    assert_eq!(
        failure.source(),
        service_daemon::__private::ProviderInitSourceKind::Panic
    );
}

#[derive(Clone, Debug)]
#[provider_contract]
struct CancellationContract;

#[provider_impl]
async fn cancellation_contract_impl() -> CancellationContract {
    CancellationContract
}

#[tokio::test]
async fn provider_contract_preserves_candidate_cancellation_source() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let result = service_daemon::__private::resolve_provider_contract::<CancellationContract>(
        "CancellationContract",
        short_policy(),
        cancel,
    )
    .await;

    let failure = result.expect_err("cancelled candidate chain should fail");
    assert_eq!(
        failure.source(),
        service_daemon::__private::ProviderInitSourceKind::Cancelled
    );
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[provider_contract]
struct NoCandidateContract;

#[tokio::test]
async fn provider_contract_without_candidates_is_fatal() {
    let result = NoCandidateContract::resolve().await;

    match result {
        Err(ProviderInitError::Fatal { provider, message }) => {
            assert_eq!(provider, "NoCandidateContract");
            assert!(message.contains("no registered #[provider_impl] candidates"));
        }
        other => panic!("expected no-candidate fatal, got {other:?}"),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[provider_contract]
struct ManagedContract(u32);

#[provider_impl]
async fn managed_contract_impl() -> ManagedContract {
    ManagedContract(7)
}

#[tokio::test]
async fn provider_contract_supports_managed_state_and_watch() {
    let watch = <ManagedContract as service_daemon::WatchableProvided>::watch_dependency();
    let lock = ManagedContract::resolve_rwlock()
        .await
        .expect("managed contract should resolve");

    {
        let mut guard = lock.write().await;
        *guard = ManagedContract(11);
    }

    let change = tokio::time::timeout(Duration::from_secs(1), watch.changed())
        .await
        .expect("watch should observe managed contract mutation");
    assert_eq!(
        change.reason,
        service_daemon::ProviderDependencyChangeReason::Value
    );

    let snapshot = ManagedContract::resolve()
        .await
        .expect("managed contract snapshot should resolve");
    assert_eq!(snapshot.0, 11);
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[provider_contract]
struct DependencyLeaf(u32);

#[provider_impl]
async fn dependency_leaf_impl() -> DependencyLeaf {
    DependencyLeaf(3)
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[provider_contract]
struct DependencyParent(u32);

#[provider_impl]
async fn dependency_parent_impl(leaf: Arc<DependencyLeaf>) -> DependencyParent {
    DependencyParent(leaf.0 + 4)
}

#[tokio::test]
async fn provider_contract_candidate_dependencies_are_resolved() {
    let value = DependencyParent::resolve()
        .await
        .expect("candidate dependency should resolve");

    assert_eq!(value.0, 7);
}

#[derive(Clone, Debug)]
struct UnselectedFatalDependency;

static UNSELECTED_FATAL_DEPENDENCY_INITS: AtomicU32 = AtomicU32::new(0);

#[provider]
async fn unselected_fatal_dependency() -> Result<UnselectedFatalDependency, ProviderError> {
    UNSELECTED_FATAL_DEPENDENCY_INITS.fetch_add(1, Ordering::SeqCst);
    Err(ProviderError::Fatal(
        "unselected candidate dependency must not initialize".to_owned(),
    ))
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[provider_contract]
struct LazyCandidateDependencyContract(&'static str);

#[provider_impl(priority = 90)]
async fn selected_candidate_without_dependency() -> LazyCandidateDependencyContract {
    LazyCandidateDependencyContract("selected")
}

#[provider_impl(priority = 10)]
async fn unselected_candidate_with_fatal_dependency(
    _dependency: Arc<UnselectedFatalDependency>,
) -> LazyCandidateDependencyContract {
    LazyCandidateDependencyContract("unselected")
}

#[tokio::test]
async fn provider_contract_does_not_initialize_unselected_candidate_dependencies() {
    let value = LazyCandidateDependencyContract::resolve()
        .await
        .expect("higher-priority candidate should resolve without preparing fallback dependencies");

    assert_eq!(value.0, "selected");
    assert_eq!(UNSELECTED_FATAL_DEPENDENCY_INITS.load(Ordering::SeqCst), 0);
}

#[derive(Clone, Debug)]
struct DeferredFallbackDependency;

static DEFERRED_FALLBACK_DEPENDENCY_INITS: AtomicU32 = AtomicU32::new(0);

#[provider]
async fn deferred_fallback_dependency() -> DeferredFallbackDependency {
    DEFERRED_FALLBACK_DEPENDENCY_INITS.fetch_add(1, Ordering::SeqCst);
    DeferredFallbackDependency
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[provider_contract]
struct DeferredFallbackContract(&'static str);

#[provider_impl(priority = 90)]
async fn unavailable_before_deferred_fallback() -> Result<DeferredFallbackContract, ProviderError> {
    assert_eq!(
        DEFERRED_FALLBACK_DEPENDENCY_INITS.load(Ordering::SeqCst),
        0,
        "fallback dependency must remain lazy until its candidate is attempted"
    );
    Err(ProviderError::Unavailable(
        "primary implementation does not apply".to_owned(),
    ))
}

#[provider_impl(priority = 10)]
async fn deferred_fallback_with_dependency(
    _dependency: Arc<DeferredFallbackDependency>,
) -> DeferredFallbackContract {
    DeferredFallbackContract("fallback")
}

#[tokio::test]
async fn provider_contract_initializes_fallback_dependencies_only_after_unavailable() {
    let value = DeferredFallbackContract::resolve()
        .await
        .expect("fallback candidate should resolve after primary is unavailable");

    assert_eq!(value.0, "fallback");
    assert_eq!(DEFERRED_FALLBACK_DEPENDENCY_INITS.load(Ordering::SeqCst), 1);
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[provider_contract]
struct EqualPriorityContract(&'static str);

#[provider_impl(priority = 50)]
async fn equal_priority_alpha() -> EqualPriorityContract {
    EqualPriorityContract("alpha")
}

#[provider_impl(priority = 50)]
async fn equal_priority_beta() -> EqualPriorityContract {
    EqualPriorityContract("beta")
}

#[tokio::test]
async fn provider_contract_breaks_equal_priority_ties_by_module_and_name() {
    let value = EqualPriorityContract::resolve()
        .await
        .expect("equal-priority candidates should resolve deterministically");

    assert_eq!(value.0, "alpha");
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[provider_contract]
struct MutexContract(u32);

#[provider_impl]
async fn mutex_contract_impl() -> MutexContract {
    MutexContract(13)
}

#[tokio::test]
async fn provider_contract_supports_mutex_managed_state() {
    let lock = MutexContract::resolve_mutex()
        .await
        .expect("provider contract should resolve as managed Mutex state");

    let mut guard = lock.lock().await;
    assert_eq!(guard.0, 13);
    guard.0 = 21;
}

static DAEMON_LOCAL_CONTRACT_INITS: AtomicU32 = AtomicU32::new(0);
static DAEMON_LOCAL_CONTRACT_OBSERVATIONS: AtomicU32 = AtomicU32::new(0);
static DAEMON_LOCAL_CONTRACT_FIRST_VALUE: AtomicU32 = AtomicU32::new(0);
static DAEMON_LOCAL_CONTRACT_SECOND_VALUE: AtomicU32 = AtomicU32::new(0);

#[service(tags = ["provider_contract_daemon_local"])]
async fn daemon_local_contract_target() -> anyhow::Result<()> {
    done();
    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(5)).await;
    }
    Ok(())
}

#[derive(Clone)]
#[provider_contract]
struct DaemonLocalContract {
    sequence: u32,
}

#[provider_impl(priority = 90)]
async fn daemon_local_contract_primary() -> DaemonLocalContract {
    let sequence = DAEMON_LOCAL_CONTRACT_INITS.fetch_add(1, Ordering::SeqCst) + 1;
    DaemonLocalContract { sequence }
}

#[provider_impl(priority = 10)]
async fn daemon_local_contract_handle_fallback() -> Result<DaemonLocalContract, ProviderError> {
    service_handle!(daemon_local_contract_target).map(|_target| DaemonLocalContract { sequence: 0 })
}

#[service(tags = ["provider_contract_daemon_local"])]
async fn daemon_local_contract_consumer(contract: Arc<DaemonLocalContract>) -> anyhow::Result<()> {
    let observation = DAEMON_LOCAL_CONTRACT_OBSERVATIONS.fetch_add(1, Ordering::SeqCst);
    match observation {
        0 => DAEMON_LOCAL_CONTRACT_FIRST_VALUE.store(contract.sequence, Ordering::SeqCst),
        1 => DAEMON_LOCAL_CONTRACT_SECOND_VALUE.store(contract.sequence, Ordering::SeqCst),
        _ => {}
    }
    done();
    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(5)).await;
    }
    Ok(())
}

static INHERITED_CONTRACT_INITS: AtomicU32 = AtomicU32::new(0);
static INHERITED_CONTRACT_OBSERVATIONS: AtomicU32 = AtomicU32::new(0);
static INHERITED_CONTRACT_FIRST_VALUE: AtomicU32 = AtomicU32::new(0);
static INHERITED_CONTRACT_SECOND_VALUE: AtomicU32 = AtomicU32::new(0);

#[derive(Clone)]
#[provider_contract]
struct InheritedContract(u32);

#[provider_impl]
async fn inherited_contract_impl() -> InheritedContract {
    InheritedContract(INHERITED_CONTRACT_INITS.fetch_add(1, Ordering::SeqCst) + 1)
}

#[service(tags = ["provider_contract_inherited"])]
async fn inherited_contract_consumer(contract: Arc<InheritedContract>) -> anyhow::Result<()> {
    let observation = INHERITED_CONTRACT_OBSERVATIONS.fetch_add(1, Ordering::SeqCst);
    match observation {
        0 => INHERITED_CONTRACT_FIRST_VALUE.store(contract.0, Ordering::SeqCst),
        1 => INHERITED_CONTRACT_SECOND_VALUE.store(contract.0, Ordering::SeqCst),
        _ => {}
    }
    done();
    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(5)).await;
    }
    Ok(())
}

async fn run_tagged_daemon_until_observed(tag: &'static str, observed: &AtomicU32, count: u32) {
    let daemon = ServiceDaemon::builder()
        .with_registry(Registry::builder().with_tag(tag).build())
        .with_restart_policy(RestartPolicy::for_testing())
        .build();
    let cancel = daemon.cancel_token();

    daemon.run().await;
    timeout(Duration::from_secs(5), async {
        while observed.load(Ordering::SeqCst) < count {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("contract consumer should observe provider before timeout");

    cancel.cancel();
    timeout(Duration::from_secs(5), daemon.wait())
        .await
        .expect("daemon shutdown should not time out")
        .expect("daemon shutdown should succeed");
}

#[tokio::test]
async fn service_handle_contract_isolated_between_daemons() {
    run_tagged_daemon_until_observed(
        "provider_contract_daemon_local",
        &DAEMON_LOCAL_CONTRACT_OBSERVATIONS,
        1,
    )
    .await;
    run_tagged_daemon_until_observed(
        "provider_contract_daemon_local",
        &DAEMON_LOCAL_CONTRACT_OBSERVATIONS,
        2,
    )
    .await;

    assert_eq!(DAEMON_LOCAL_CONTRACT_INITS.load(Ordering::SeqCst), 2);
    assert!(DAEMON_LOCAL_CONTRACT_FIRST_VALUE.load(Ordering::SeqCst) > 0);
    assert!(DAEMON_LOCAL_CONTRACT_SECOND_VALUE.load(Ordering::SeqCst) > 0);
}

#[tokio::test]
async fn contract_without_service_handle_shares_root_between_daemons() {
    run_tagged_daemon_until_observed(
        "provider_contract_inherited",
        &INHERITED_CONTRACT_OBSERVATIONS,
        1,
    )
    .await;
    run_tagged_daemon_until_observed(
        "provider_contract_inherited",
        &INHERITED_CONTRACT_OBSERVATIONS,
        2,
    )
    .await;

    assert_eq!(INHERITED_CONTRACT_INITS.load(Ordering::SeqCst), 1);
    assert_eq!(INHERITED_CONTRACT_FIRST_VALUE.load(Ordering::SeqCst), 1);
    assert_eq!(INHERITED_CONTRACT_SECOND_VALUE.load(Ordering::SeqCst), 1);
}

static REACHABLE_EAGER_CONTRACT_INITS: AtomicU32 = AtomicU32::new(0);
static UNREACHABLE_EAGER_CONTRACT_INITS: AtomicU32 = AtomicU32::new(0);

#[derive(Clone)]
#[provider_contract(eager = true)]
struct ReachableEagerContract;

#[provider_impl]
async fn reachable_eager_contract_impl() -> ReachableEagerContract {
    REACHABLE_EAGER_CONTRACT_INITS.fetch_add(1, Ordering::SeqCst);
    ReachableEagerContract
}

#[service(tags = ["provider_contract_eager_reachable"])]
async fn reachable_eager_contract_service(
    _contract: Arc<ReachableEagerContract>,
) -> anyhow::Result<()> {
    done();
    Ok(())
}

#[derive(Clone)]
#[provider_contract(eager = true)]
struct UnreachableEagerContract;

#[provider_impl]
async fn unreachable_eager_contract_impl() -> UnreachableEagerContract {
    UNREACHABLE_EAGER_CONTRACT_INITS.fetch_add(1, Ordering::SeqCst);
    UnreachableEagerContract
}

#[service(tags = ["provider_contract_eager_unreachable"])]
async fn unreachable_eager_contract_service(
    _contract: Arc<UnreachableEagerContract>,
) -> anyhow::Result<()> {
    done();
    Ok(())
}

#[tokio::test]
async fn provider_contract_eager_init_respects_service_reachability() {
    let daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("provider_contract_eager_reachable")
                .build(),
        )
        .with_restart_policy(RestartPolicy::for_testing())
        .build();
    let cancel = daemon.cancel_token();

    daemon.run().await;
    assert_eq!(REACHABLE_EAGER_CONTRACT_INITS.load(Ordering::SeqCst), 1);
    assert_eq!(UNREACHABLE_EAGER_CONTRACT_INITS.load(Ordering::SeqCst), 0);

    cancel.cancel();
    timeout(Duration::from_secs(5), daemon.wait())
        .await
        .expect("daemon shutdown should not time out")
        .expect("daemon shutdown should succeed");
}

#[cfg(feature = "simulation")]
static SIMULATION_CONTRACT_INITS: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "simulation")]
static SIMULATION_CONTRACT_VALUE: AtomicU32 = AtomicU32::new(0);

#[cfg(feature = "simulation")]
#[derive(Clone)]
#[provider_contract(eager = true)]
struct SimulationContract(u32);

#[cfg(feature = "simulation")]
#[provider_impl]
async fn simulation_contract_impl() -> SimulationContract {
    SIMULATION_CONTRACT_INITS.fetch_add(1, Ordering::SeqCst);
    SimulationContract(1)
}

#[cfg(feature = "simulation")]
#[service(tags = ["provider_contract_simulation_override"])]
async fn simulation_contract_service(contract: Arc<SimulationContract>) -> anyhow::Result<()> {
    SIMULATION_CONTRACT_VALUE.store(contract.0, Ordering::SeqCst);
    done();
    Ok(())
}

#[cfg(feature = "simulation")]
#[tokio::test]
async fn simulation_override_bypasses_contract_candidate_initialization() {
    use service_daemon::MockContext;

    let simulation = MockContext::builder()
        .with_logging(false)
        .with_provider_override(SimulationContract(42))
        .with_registry(
            Registry::builder()
                .with_tag("provider_contract_simulation_override")
                .build(),
        )
        .build();

    simulation
        .run_for_duration(Duration::from_millis(100))
        .await
        .expect("simulation contract topology should run");

    assert_eq!(SIMULATION_CONTRACT_VALUE.load(Ordering::SeqCst), 42);
    assert_eq!(SIMULATION_CONTRACT_INITS.load(Ordering::SeqCst), 0);
}
