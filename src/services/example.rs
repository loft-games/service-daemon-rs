use service_daemon::service;
use std::sync::Arc;
use tracing::info;

#[service]
pub async fn example_service(port: Arc<i32>, db_url: Arc<String>) -> anyhow::Result<()> {
    info!(
        "Example service running on port {} with DB {}",
        port, db_url
    );
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
        info!("Heartbeat from example service");
    }
}
