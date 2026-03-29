use anyhow::Result;
use service_daemon::{service, ServicePriority};
use std::thread;
use std::time::Duration;
use tokio::time;
use tracing::info;

/// Simulates the 50ms Modbus server, running in an isolated thread.
#[service(priority = ServicePriority::STORAGE, scheduling = Isolated)]
pub async fn modbus_server() -> Result<()> {
    let thread = thread::current();
    let thread_id = thread.id();
    let thread_name = thread.name().unwrap_or("unnamed").to_string();

    info!(
        "[Isolated] Modbus server (50ms) running on thread {:?} ({})",
        thread_id, thread_name
    );

    // Simulate high-frequency loop
    let mut interval = time::interval(Duration::from_millis(50));
    loop {
        interval.tick().await;
    }
}
