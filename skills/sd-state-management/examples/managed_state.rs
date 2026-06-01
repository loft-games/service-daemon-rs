// Managed state: share mutable state across services, react with a Watch trigger,
// and persist across restarts with the Shelf.
use service_daemon::core::managed_state::RwLock;
use service_daemon::{service, trigger};
use std::sync::Arc; // tracked RwLock wrapper

// Declare the state type as a provider (DI plumbing is generated).
#[service_daemon::provider]
#[derive(Default, Clone, Debug)]
pub struct MetricsData {
    pub requests: u64,
}

// A writer mutates the managed state. Dropping the write guard publishes the change.
#[service]
pub async fn collector(metrics: Arc<RwLock<MetricsData>>) -> anyhow::Result<()> {
    while !service_daemon::is_shutdown() {
        if !service_daemon::sleep(std::time::Duration::from_secs(1)).await {
            break;
        }
        {
            let mut guard = metrics.write().await;
            guard.requests += 1;
        } // publication happens here
    }
    Ok(())
}

// A Watch trigger fires when MetricsData changes and gets the new snapshot.
#[trigger(Watch(MetricsData))]
pub async fn on_metrics_changed(snapshot: Arc<MetricsData>) -> anyhow::Result<()> {
    println!("requests now {}", snapshot.requests);
    Ok(())
}

// Persist a checkpoint that must survive a restart of THIS service.
#[service]
pub async fn checkpointer() -> anyhow::Result<()> {
    // Inherit any checkpoint left by the previous generation (keyed, async).
    let resumed: Option<MetricsData> = service_daemon::unshelve("metrics").await;
    let mut state = resumed.unwrap_or_default();

    while !service_daemon::is_shutdown() {
        if !service_daemon::sleep(std::time::Duration::from_secs(5)).await {
            break;
        }
        state.requests += 1;
        // Deposit a clone under our key so the next generation can resume from here.
        service_daemon::shelve("metrics", state.clone()).await;
    }
    Ok(())
}
