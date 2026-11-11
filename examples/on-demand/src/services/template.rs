//! Service templates selected by the daemon without auto-starting.

use service_daemon::{done, service, sleep};
use std::time::Duration;
use tracing::info;

pub struct WorkerConfig {
    pub id: u64,
    pub heartbeat_interval: Duration,
}

#[service(tags = ["on-demand"])]
pub async fn worker(#[input] config: &WorkerConfig) -> anyhow::Result<()> {
    done();
    info!(worker_id = config.id, "worker instance started");
    while sleep(config.heartbeat_interval).await {
        info!(worker_id = config.id, "worker instance heartbeat");
    }
    info!(worker_id = config.id, "worker instance stopped");
    Ok(())
}
