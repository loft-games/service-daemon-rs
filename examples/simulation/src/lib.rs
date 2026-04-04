//! Simulation example service definitions.
//!
//! These `#[service]`-annotated functions are the real services used in
//! simulation integration tests.

use std::time::Duration;

use service_daemon::{service, shelve, state, unshelve};

// =============================================================================
// Real services defined with #[service] macro
// =============================================================================

/// A real service that reads shelf data and reacts to status changes.
///
/// In a production environment, this could be a database connection manager,
/// a config watcher, or any service with real business logic.
///
/// Tagged with "sim_shelf" for selective inclusion in simulation tests.
#[service(tags = ["sim_shelf"])]
pub async fn shelf_reader_service() -> anyhow::Result<()> {
    tracing::info!("shelf_reader_service started, status = {:?}", state());

    // Try to read pre-filled shelf data (injected by MockContext)
    let config: Option<String> = unshelve("config_key").await;
    tracing::info!("shelf_reader_service: unshelved config_key = {:?}", config);

    if let Some(val) = config {
        // Signal to the test that we successfully read the value.
        // We persist it back to the shelf under a "result" key for the test to verify.
        shelve("read_result", val).await;
    }

    // Keep running until shutdown, periodically checking for SimulationHandle mutations.
    loop {
        if service_daemon::is_shutdown() {
            break;
        }

        // Check for dynamically injected values (SimulationHandle phase 2)
        let dynamic_val: Option<String> = unshelve("dynamic_key").await;
        if let Some(val) = dynamic_val {
            tracing::info!(
                "shelf_reader_service: SimulationHandle injected dynamic_key = {:?}",
                val
            );
            shelve("dynamic_result", val).await;
        }

        service_daemon::sleep(Duration::from_millis(50)).await;
    }

    Ok(())
}

/// A real service that monitors its own lifecycle status via `state()`.
///
/// Tagged with "sim_status" for selective inclusion in simulation tests.
#[service(tags = ["sim_status"])]
pub async fn status_watcher_service() -> anyhow::Result<()> {
    tracing::info!(
        "status_watcher_service started, initial status = {:?}",
        state()
    );

    loop {
        let current_status = state();

        // Persist the observed status to shelf for the test to verify.
        shelve("observed_status", format!("{:?}", current_status)).await;

        if service_daemon::is_shutdown() {
            break;
        }

        service_daemon::sleep(Duration::from_millis(50)).await;
    }

    Ok(())
}
