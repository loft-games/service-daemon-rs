//! Framework-aware cadence services for public lifecycle validation.
use service_daemon::service;
use std::time::Duration;

#[service(tags = ["cadence-validation"])]
pub async fn standard_cadence() -> anyhow::Result<()> {
    while service_daemon::sleep(Duration::from_millis(5)).await {
        tracing::trace!("standard cadence tick");
    }
    Ok(())
}

#[service(scheduling = HighPriority, tags = ["cadence-validation"])]
pub async fn high_priority_cadence() -> anyhow::Result<()> {
    while service_daemon::sleep(Duration::from_millis(5)).await {
        tracing::trace!("high-priority cadence tick");
    }
    Ok(())
}
