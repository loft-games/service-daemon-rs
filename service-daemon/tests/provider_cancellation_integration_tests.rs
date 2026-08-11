use service_daemon::{
    ProviderError, Registry, RestartPolicy, ServiceDaemon, TT::*, provider, service, trigger,
};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};
use tokio::time::timeout;

static PREBODY_PROVIDER_ATTEMPTS: AtomicU32 = AtomicU32::new(0);
static PREBODY_SERVICE_ENTERED: AtomicBool = AtomicBool::new(false);
static INFLIGHT_PROVIDER_ATTEMPTS: AtomicU32 = AtomicU32::new(0);
static INFLIGHT_SERVICE_ENTERED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Default)]
pub struct PrebodyBlockingProvider;

#[provider]
async fn prebody_blocking_provider() -> std::result::Result<PrebodyBlockingProvider, ProviderError>
{
    PREBODY_PROVIDER_ATTEMPTS.fetch_add(1, Ordering::SeqCst);
    Err(ProviderError::Retryable(
        "pre-body provider intentionally blocked".to_string(),
    ))
}

#[service(tags = ["__test_provider_prebody_cancellation__"])]
async fn service_waiting_on_provider(
    _provider: std::sync::Arc<PrebodyBlockingProvider>,
) -> anyhow::Result<()> {
    PREBODY_SERVICE_ENTERED.store(true, Ordering::SeqCst);
    service_daemon::done();
    Ok(())
}

#[derive(Clone, Default)]
pub struct InflightBlockingProvider;

#[provider]
async fn inflight_blocking_provider() -> std::result::Result<InflightBlockingProvider, ProviderError>
{
    INFLIGHT_PROVIDER_ATTEMPTS.fetch_add(1, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(30)).await;
    Err(ProviderError::Retryable(
        "in-flight provider intentionally blocked".to_string(),
    ))
}

#[service(tags = ["__test_provider_inflight_cancellation__"])]
async fn service_waiting_on_inflight_provider(
    _provider: std::sync::Arc<InflightBlockingProvider>,
) -> anyhow::Result<()> {
    INFLIGHT_SERVICE_ENTERED.store(true, Ordering::SeqCst);
    service_daemon::done();
    Ok(())
}

#[provider(Notify)]
pub struct CancellationTrigger;

#[trigger(Event(CancellationTrigger), tags = ["__test_trigger_prebody_cancellation__"])]
async fn trigger_waiting_on_provider(
    _provider: std::sync::Arc<PrebodyBlockingProvider>,
) -> anyhow::Result<()> {
    PREBODY_SERVICE_ENTERED.store(true, Ordering::SeqCst);
    Ok(())
}

fn test_policy() -> RestartPolicy {
    RestartPolicy::builder()
        .initial_delay(Duration::from_millis(20))
        .max_delay(Duration::from_millis(50))
        .jitter_factor(0.0)
        .wave_spawn_timeout(Duration::from_millis(100))
        .provider_init_timeout(Duration::from_secs(2))
        .wave_stop_timeout(Duration::from_millis(200))
        .build()
}

#[tokio::test]
async fn test_service_prebody_provider_resolve_respects_shutdown() -> anyhow::Result<()> {
    PREBODY_PROVIDER_ATTEMPTS.store(0, Ordering::SeqCst);
    PREBODY_SERVICE_ENTERED.store(false, Ordering::SeqCst);

    let daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__test_provider_prebody_cancellation__")
                .build(),
        )
        .with_restart_policy(test_policy())
        .build();

    let cancel = daemon.cancel_token();
    daemon.run().await;

    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(
        PREBODY_PROVIDER_ATTEMPTS.load(Ordering::SeqCst) > 0,
        "provider resolve never started"
    );
    assert!(
        !PREBODY_SERVICE_ENTERED.load(Ordering::SeqCst),
        "service body should not run before dependency resolve succeeds"
    );

    let shutdown_started = Instant::now();
    cancel.cancel();
    timeout(Duration::from_millis(700), daemon.wait()).await??;

    assert!(
        shutdown_started.elapsed() < Duration::from_secs(1),
        "daemon shutdown took too long while provider resolve was blocked"
    );
    assert!(
        !PREBODY_SERVICE_ENTERED.load(Ordering::SeqCst),
        "service body unexpectedly entered during blocked provider resolve"
    );

    Ok(())
}

#[tokio::test]
async fn test_trigger_prebody_provider_resolve_respects_shutdown() -> anyhow::Result<()> {
    PREBODY_PROVIDER_ATTEMPTS.store(0, Ordering::SeqCst);
    PREBODY_SERVICE_ENTERED.store(false, Ordering::SeqCst);

    let daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__test_trigger_prebody_cancellation__")
                .build(),
        )
        .with_restart_policy(test_policy())
        .build();

    let cancel = daemon.cancel_token();
    daemon.run().await;

    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(
        PREBODY_PROVIDER_ATTEMPTS.load(Ordering::SeqCst) > 0,
        "trigger provider resolve never started"
    );
    assert!(
        !PREBODY_SERVICE_ENTERED.load(Ordering::SeqCst),
        "trigger body should not run before dependency resolve succeeds"
    );

    let shutdown_started = Instant::now();
    cancel.cancel();
    timeout(Duration::from_millis(700), daemon.wait()).await??;

    assert!(
        shutdown_started.elapsed() < Duration::from_secs(1),
        "daemon shutdown took too long while trigger provider resolve was blocked"
    );
    assert!(
        !PREBODY_SERVICE_ENTERED.load(Ordering::SeqCst),
        "trigger body unexpectedly entered during blocked provider resolve"
    );

    Ok(())
}

#[tokio::test]
async fn test_service_inflight_provider_attempt_respects_shutdown() -> anyhow::Result<()> {
    INFLIGHT_PROVIDER_ATTEMPTS.store(0, Ordering::SeqCst);
    INFLIGHT_SERVICE_ENTERED.store(false, Ordering::SeqCst);

    let daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__test_provider_inflight_cancellation__")
                .build(),
        )
        .with_restart_policy(test_policy())
        .build();

    let cancel = daemon.cancel_token();
    daemon.run().await;

    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(
        INFLIGHT_PROVIDER_ATTEMPTS.load(Ordering::SeqCst) > 0,
        "in-flight provider attempt never started"
    );
    assert!(
        !INFLIGHT_SERVICE_ENTERED.load(Ordering::SeqCst),
        "service body should not run before in-flight dependency resolve succeeds"
    );

    let shutdown_started = Instant::now();
    cancel.cancel();
    timeout(Duration::from_millis(700), daemon.wait()).await??;

    assert!(
        shutdown_started.elapsed() < Duration::from_secs(1),
        "daemon shutdown took too long while provider init future was still running"
    );
    assert!(
        !INFLIGHT_SERVICE_ENTERED.load(Ordering::SeqCst),
        "service body unexpectedly entered during in-flight provider init"
    );

    Ok(())
}
