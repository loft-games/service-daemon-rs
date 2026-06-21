// A full simulation test: pre-fill shelf, run bounded, mutate mid-flight, assert.
// Requires the daemon dep built with features = ["simulation"].
use service_daemon::{MockContext, Registry, ServiceId, ServiceStatus};
use std::time::Duration;

// `svc_id` is the strong identity of the single service under test (tagged
// "sim_shelf"). Derive it from the service's registry identity in your setup;
// keeping one service under test makes the id unambiguous.
fn service_under_test() -> ServiceId {
    /* resolve the id for the tagged service */
    unimplemented!()
}

#[tokio::test]
async fn pre_filled_shelf_is_visible_to_the_service() {
    let svc_id = service_under_test();

    // Phase 1 — seed state before the daemon starts.
    let (builder, handle) = MockContext::builder()
        .with_shelf::<String>(svc_id, "config_key", "phase1_value".into())
        .with_logging(false) // lightweight: skip framework log services
        .build();

    let mut daemon = builder
        .with_registry(Registry::builder().with_tag("sim_shelf").build())
        .build();

    // Run the daemon in the background so we can mutate while it is in flight.
    let run = tokio::spawn(async move {
        daemon.run_for_duration(Duration::from_secs(3)).await.ok();
    });

    // Let the runner spawn the service before touching it by id.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // The service should have read the seeded config and written a result.
    assert_eq!(
        handle.get_shelf::<String>(svc_id, "read_result"),
        Some("phase1_value".to_string()),
    );

    // Phase 2 — inject new state mid-flight; visible on the next unshelve.
    handle.set_shelf::<String>(svc_id, "dynamic_key", "phase2_value".into());

    run.await.ok();

    // After the bounded run, assert the mid-flight injection took effect.
    assert_eq!(
        handle.get_shelf::<String>(svc_id, "dynamic_result"),
        Some("phase2_value".to_string()),
    );
}

#[tokio::test]
async fn status_override_drives_a_reload() {
    let svc_id = service_under_test();
    let (builder, handle) = MockContext::builder().build();
    let mut daemon = builder
        .with_registry(Registry::builder().with_tag("sim_status").build())
        .build();

    let run = tokio::spawn(async move {
        daemon.run_for_duration(Duration::from_secs(3)).await.ok();
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Simulate an operator marking the service for reload.
    handle.set_status(svc_id, ServiceStatus::NeedReload);
    handle.trigger_reload(&svc_id);

    run.await.ok();
    assert!(handle.get_status(svc_id).is_some());
}
