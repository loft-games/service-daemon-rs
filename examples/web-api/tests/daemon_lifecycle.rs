use example_web_api as _;
use service_daemon::{RestartPolicy, ServiceDaemon};
use std::time::Duration;

#[tokio::test]
async fn daemon_shutdown_records_diagnostics() -> anyhow::Result<()> {
    let daemon = ServiceDaemon::builder()
        .with_restart_policy(RestartPolicy::for_testing())
        .build();
    let cancel = daemon.cancel_token();

    daemon.run().await;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if daemon
                .diagnostics_snapshot()
                .services
                .iter()
                .any(|service| service.service_name == "http_server_service")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;

    cancel.cancel();
    daemon.wait().await?;

    let service = daemon
        .diagnostics_snapshot()
        .services
        .into_iter()
        .find(|service| service.service_name == "http_server_service")
        .expect("HTTP service diagnostics should be recorded");
    assert!(
        service.aggregate.lifecycle.shutdown >= 1
            || service.aggregate.lifecycle.normal_exit >= 1
            || service.aggregate.lifecycle.terminated >= 1,
        "HTTP service should record a shutdown-related lifecycle fact: {:?}",
        service.aggregate.lifecycle
    );

    Ok(())
}
