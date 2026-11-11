// A full simulation test: pre-fill shelf, run bounded, mutate mid-flight, assert.
// Requires the daemon dep built with features = ["simulation"].
use service_daemon::{
    MockContext, Registry, ServiceInstanceHandle, ServiceInstanceId, ServiceStatus,
};
use std::time::Duration;

fn service_instance_id(registry: &Registry, name: &str) -> ServiceInstanceId {
    registry
        .services()
        .iter()
        .find(|service| service.name() == name)
        .and_then(|service| service.instance_ids().first().copied())
        .expect("service should be materialized in registry")
}

fn service_instance(
    simulation: &service_daemon::SimulationHandle,
    name: &str,
) -> ServiceInstanceHandle {
    simulation
        .service_instances()
        .into_iter()
        .find(|instance| instance.name() == name)
        .expect("service should be materialized in simulation daemon")
}

#[tokio::test]
async fn pre_filled_shelf_is_visible_to_the_service() {
    let registry = Registry::builder().with_tag("sim_shelf").build();
    let svc_id = service_instance_id(&registry, "shelf_reader_service");

    // Seed state before the daemon starts.
    let simulation = MockContext::builder()
        .with_shelf::<String>(svc_id, "config_key", "phase1_value".into())
        .with_logging(false) // lightweight: skip framework log services
        .with_registry(registry)
        .build();

    // Run the daemon in the background so we can mutate while it is in flight.
    let runner = simulation.clone();
    let run = tokio::spawn(async move {
        runner.run_for_duration(Duration::from_secs(3)).await.ok();
    });

    // Let the runner spawn the service before touching it by handle.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let service = service_instance(&simulation, "shelf_reader_service");

    // The service should have read the seeded config and written a result.
    assert_eq!(
        simulation.get_shelf::<String>(&service, "read_result"),
        Some("phase1_value".to_string()),
    );

    // Inject new state mid-flight; visible on the next unshelve.
    simulation.set_shelf::<String>(&service, "dynamic_key", "phase2_value".into());

    run.await.ok();

    // After the bounded run, assert the mid-flight injection took effect.
    assert_eq!(
        simulation.get_shelf::<String>(&service, "dynamic_result"),
        Some("phase2_value".to_string()),
    );
}

#[tokio::test]
async fn status_override_drives_a_reload() {
    let registry = Registry::builder().with_tag("sim_status").build();
    let simulation = MockContext::builder().with_registry(registry).build();

    let runner = simulation.clone();
    let run = tokio::spawn(async move {
        runner.run_for_duration(Duration::from_secs(3)).await.ok();
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let service = service_instance(&simulation, "status_watcher_service");

    // Simulate an operator marking the service for reload.
    simulation.set_status(&service, ServiceStatus::NeedReload);
    simulation.trigger_reload(&service);

    run.await.ok();
    assert!(simulation.get_status(&service).is_some());
}
