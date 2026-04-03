use service_daemon::{ProviderError, ServiceDaemon, ServiceId, ServiceStatus};
use service_daemon_macro::provider;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::time::timeout;

static LAZY_FATAL_PROVIDER_CALLED: AtomicBool = AtomicBool::new(false);
static LAZY_HEALTHY_STARTED: AtomicBool = AtomicBool::new(false);
static LAZY_HEALTHY_STOPPED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Default)]
pub struct LazyFatalConfig;

#[provider]
async fn lazy_fatal_config() -> std::result::Result<LazyFatalConfig, ProviderError> {
    LAZY_FATAL_PROVIDER_CALLED.store(true, Ordering::SeqCst);
    Err(ProviderError::Fatal(
        "lazy provider fatal for runtime shutdown".to_string(),
    ))
}

#[service_daemon::service(priority = 100, tags = ["lazy_fatal_healthy"])]
async fn healthy_service() -> anyhow::Result<()> {
    LAZY_HEALTHY_STARTED.store(true, Ordering::SeqCst);
    service_daemon::done();

    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }

    LAZY_HEALTHY_STOPPED.store(true, Ordering::SeqCst);
    Ok(())
}

#[service_daemon::service(priority = 0, tags = ["lazy_fatal_trigger"])]
async fn fatal_service(_config: Arc<LazyFatalConfig>) -> anyhow::Result<()> {
    Ok(())
}

#[tokio::test]
async fn test_lazy_provider_fatal_triggers_daemon_shutdown() -> anyhow::Result<()> {
    LAZY_FATAL_PROVIDER_CALLED.store(false, Ordering::SeqCst);
    LAZY_HEALTHY_STARTED.store(false, Ordering::SeqCst);
    LAZY_HEALTHY_STOPPED.store(false, Ordering::SeqCst);

    let mut daemon = ServiceDaemon::builder().build();

    daemon.run().await;

    let shutdown_token = daemon.cancel_token();
    timeout(Duration::from_secs(5), shutdown_token.cancelled()).await?;
    assert!(shutdown_token.is_cancelled());

    assert!(LAZY_FATAL_PROVIDER_CALLED.load(Ordering::SeqCst));
    assert!(LAZY_HEALTHY_STARTED.load(Ordering::SeqCst));

    timeout(Duration::from_secs(5), daemon.wait()).await??;

    assert!(LAZY_HEALTHY_STOPPED.load(Ordering::SeqCst));
    assert_eq!(
        daemon.handle().get_service_status(&ServiceId::new(0)).await,
        ServiceStatus::Terminated
    );
    assert_eq!(
        daemon.handle().get_service_status(&ServiceId::new(1)).await,
        ServiceStatus::Terminated
    );

    Ok(())
}
