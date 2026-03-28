use service_daemon::{ServiceDaemon, TT::*, provider, service, trigger};
use std::time::Duration;
use tracing::info;

// --- Providers ---

#[provider(Notify)]
pub struct Signal1;

#[provider(Notify)]
pub struct Signal2;

// --- Services ---

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

// --- Triggers ---

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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Enable diagnostics & logging
    service_daemon::core::logging::init_logging();

    let mut daemon = ServiceDaemon::builder().build();

    // Start the daemon (this returns immediately)
    daemon.run().await;

    info!("Daemon running. Press Ctrl+C to stop and export the graph.");

    // Wait for the daemon to stop (handles Ctrl+C/SIGINT and auto-exports topology internally)
    let _ = daemon.wait().await;

    Ok(())
}
