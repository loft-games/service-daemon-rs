//! Integration test for the on-demand service instance example.

use example_on_demand as _;
use service_daemon::{Registry, RestartPolicy, ServiceDaemon, ServiceInstanceId};
use std::time::Duration;

#[tokio::test]
async fn on_demand_example_runs_periodic_worker_lifecycle() -> anyhow::Result<()> {
    let _ = service_daemon::try_init_logging();

    let daemon = ServiceDaemon::builder()
        .with_registry(Registry::builder().with_tag("on-demand").build())
        .with_restart_policy(RestartPolicy::for_testing())
        .build();

    daemon.run().await;
    let worker_id = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(worker) = daemon
                .service_instances()
                .into_iter()
                .find(|instance| instance.name() == "worker")
            {
                break worker.instance_id();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;

    tokio::time::timeout(
        Duration::from_secs(3),
        wait_until_instance_removed(&daemon, worker_id),
    )
    .await?;

    daemon.shutdown();
    tokio::time::timeout(Duration::from_secs(3), daemon.wait()).await??;

    Ok(())
}

async fn wait_until_instance_removed(
    daemon: &service_daemon::DaemonInstanceHandle,
    worker_id: ServiceInstanceId,
) {
    loop {
        let still_registered = daemon
            .service_instances()
            .iter()
            .any(|instance| instance.instance_id() == worker_id);
        if !still_registered {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
