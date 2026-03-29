use crate::providers::{Signal1, Signal2};
use service_daemon::{service, trigger, TT::*};
use std::time::Duration;
use tracing::info;

// 1. A producer service that periodically "pings" Signal1
#[service]
pub async fn producer_service(notifier: Arc<Signal1>) -> anyhow::Result<()> {
    info!("Producer started. Will emit signals every 1s.");
    loop {
        if !service_daemon::sleep(Duration::from_secs(1)).await {
            break;
        }
        info!("Emitting Signal1...");
        notifier.notify_one();
    }
    Ok(())
}

// 2. A trigger that reacts to Signal1 and forwards to Signal2
#[trigger(Signal(Signal1))]
pub async fn consumer_trigger(notifier2: Arc<Signal2>) -> anyhow::Result<()> {
    info!("Consumer triggered! Forwarding to Signal2...");
    // Forward the signal to show multi-hop topology
    service_daemon::sleep(Duration::from_millis(100)).await;
    notifier2.notify_one();
    Ok(())
}

// 3. A secondary trigger to demonstrate causality chains (Signal2)
#[trigger(Notify(Signal2))]
pub async fn leaf_trigger() -> anyhow::Result<()> {
    info!("Leaf triggered! Causality chain complete.");
    Ok(())
}
