//! Service templates selected by the daemon without auto-starting.

use service_daemon::{done, service, wait_shutdown};
use tracing::info;

#[service(auto_start = false, tags = ["on-demand"])]
pub async fn worker() -> anyhow::Result<()> {
    done();
    info!("worker instance started");
    wait_shutdown().await;
    info!("worker instance stopped");
    Ok(())
}
