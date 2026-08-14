// Service patterns. Dependencies are injected as Arc<T> for #[provider] types.
use service_daemon::{ServiceError, service};
use std::sync::Arc;
use std::time::Duration;

// Minimal shutdown-aware loop. The first is_shutdown()/sleep() call also serves
// as the implicit readiness handshake.
#[service]
pub async fn heartbeat(cfg: Arc<AppConfig>) -> anyhow::Result<()> {
    while !service_daemon::is_shutdown() {
        if !service_daemon::sleep(Duration::from_secs(5)).await {
            break; // shutdown arrived during the sleep
        }
    }
    Ok(())
}

// Explicit readiness handshake after non-trivial initialization, plus structured
// error classification on a resource-acquisition path.
#[service]
pub async fn listener_service(listener: Arc<ApiListener>) -> anyhow::Result<()> {
    let bound = match listener.get() {
        Ok(l) => l,
        Err(e) => return Err(ServiceError::runtime_io("clone TCP listener", e).into()),
    };
    let _addr = bound.local_addr();

    service_daemon::done(); // signal Healthy so later waves can start

    service_daemon::wait_shutdown().await;
    Ok(())
}

// Fatal vs recoverable: bad config terminates with no restart; transient errors
// return an ordinary Err and restart with backoff.
#[service]
pub async fn worker(cfg: Arc<AppConfig>) -> anyhow::Result<()> {
    if !cfg.is_valid() {
        return Err(ServiceError::Fatal("invalid worker config".into()).into());
    }
    while !service_daemon::is_shutdown() {
        if !service_daemon::sleep(Duration::from_secs(1)).await {
            break;
        }
        do_unit_of_work().await?; // a transient Err here triggers restart-with-backoff
    }
    Ok(())
}

// On-demand service template. The #[input] value is owned by each created
// service instance and borrowed by every generation of that instance.
pub struct WorkerConfig {
    pub id: u64,
    pub heartbeat_interval: Duration,
}

#[service(tags = ["on-demand"])]
pub async fn on_demand_worker(#[input] cfg: &WorkerConfig) -> anyhow::Result<()> {
    service_daemon::done();
    while service_daemon::sleep(cfg.heartbeat_interval).await {
        process_worker(cfg.id).await?;
    }
    Ok(())
}
