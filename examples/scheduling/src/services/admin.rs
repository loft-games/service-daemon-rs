use anyhow::Result;
use service_daemon::{service, ServicePriority};
use std::thread;
use std::time::Duration;
use tokio::time;
use tracing::info;

/// Simulates a standard HTTP administration service running in the shared thread pool.
#[service(priority = ServicePriority::STORAGE)]
pub async fn admin_service() -> Result<()> {
    let thread = thread::current();
    let thread_id = thread.id();
    let thread_name = thread.name().unwrap_or("unnamed").to_string();

    info!(
        "[Standard] Admin service running on thread {:?} ({})",
        thread_id, thread_name
    );

    // Simulate continuous operation
    time::sleep(Duration::from_secs(3600)).await;
    Ok(())
}
