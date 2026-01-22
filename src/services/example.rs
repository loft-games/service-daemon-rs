use crate::providers::trigger_providers::{TaskQueue, UserNotifier};
use crate::providers::typed_providers::{DbUrl, Port};
use service_daemon::service;
use std::sync::Arc;
use tracing::info;

#[service]
pub async fn example_service(port: Arc<Port>, db_url: Arc<DbUrl>) -> anyhow::Result<()> {
    // No .0 needed - Display is auto-generated!
    info!(
        "Example service running on port {} with DB {}",
        port, db_url
    );
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
        info!("Heartbeat from example service");

        // --- NEW: Template-Based Interaction ---

        // 1. Trigger a Signal (Custom Trigger) from here
        // No need to inject UserNotifier as a dependency if you just want to call it!
        UserNotifier::notify();

        // 2. Push to a Broadcast Queue (all handlers will receive this message)
        let _ = TaskQueue::push("Message from service".to_owned());
    }
}
