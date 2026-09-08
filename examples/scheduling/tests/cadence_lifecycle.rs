//! Real example services exercise observation and orderly public shutdown.
use examples_scheduling as _;
use service_daemon::{Registry, ServiceDaemon};
use std::time::Duration;

#[tokio::test]
async fn cadence_services_observe_sleeps_and_shutdown() -> anyhow::Result<()> {
    let daemon = ServiceDaemon::builder()
        .with_registry(Registry::builder().with_tag("cadence-validation").build())
        .build();
    daemon.run().await;
    let outcome = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let instances = daemon.service_instances();
            let diagnostics = daemon.diagnostics_snapshot();
            if instances.len() == 2
                && instances.iter().all(|instance| {
                    diagnostics.services.iter().any(|s| {
                        s.service_instance_id == instance.instance_id()
                            && s.aggregate.service_sleep.completed >= 3
                    })
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    daemon.shutdown();
    tokio::time::timeout(Duration::from_secs(5), daemon.wait()).await??;
    outcome?;
    Ok(())
}
