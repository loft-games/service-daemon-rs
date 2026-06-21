use std::sync::Arc;

use crate::models::api::response::MaintenanceOutcome;
use crate::providers::{ExampleConfig, MaintenanceSchedule, SharedExampleState};
use service_daemon::TT::*;
use service_daemon::trigger;
use tracing::info;

#[trigger(Cron(MaintenanceSchedule))]
pub async fn maintenance_trigger(
    config: Arc<ExampleConfig>,
    state: Arc<SharedExampleState>,
) -> anyhow::Result<()> {
    run_maintenance(config, state).await?;
    Ok(())
}

pub async fn run_maintenance(
    config: Arc<ExampleConfig>,
    state: Arc<SharedExampleState>,
) -> anyhow::Result<MaintenanceOutcome> {
    let mut state_guard = state.inner.write().await;
    let mut archived = 0;
    for item in state_guard
        .items
        .values_mut()
        .filter(|item| item.stale && !item.archived)
        .take(config.maintenance_batch_size)
    {
        item.archived = true;
        item.revision += 1;
        archived += 1;
    }

    let mut refreshed = 0;
    for item in state_guard
        .items
        .values_mut()
        .filter(|item| !item.archived)
        .take(config.maintenance_batch_size)
    {
        item.stale = false;
        item.revision += 1;
        refreshed += 1;
    }

    state_guard.maintenance_runs += 1;
    state_guard.archived_total += archived;
    state_guard.refreshed_total += refreshed;
    let runs = state_guard.maintenance_runs;
    drop(state_guard);

    info!(
        archived,
        refreshed, runs, "completed Web API example maintenance"
    );

    Ok(MaintenanceOutcome {
        archived,
        refreshed,
        runs,
    })
}
