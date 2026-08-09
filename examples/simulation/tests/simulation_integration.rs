//! Integration tests for the Simulation example.
//!
//! These tests verify end-to-end behavior of `MockContext`, `SimulationHandle`,
//! and real `#[service]`-annotated functions running inside a sandbox daemon.

// Import library crate so that `#[service]` registrations participate in linkme.
use example_simulation as _;
use service_daemon::{
    __private::SERVICE_REGISTRY, MockContext, Registry, ServiceEntryId, ServiceInstanceId,
    ServiceStatus,
};
use std::time::Duration;

fn service_entry_id(name: &str) -> ServiceEntryId {
    SERVICE_REGISTRY
        .iter()
        .enumerate()
        .find_map(|(idx, entry)| (entry.name == name).then_some(ServiceEntryId::new(idx)))
        .expect("service should be registered")
}

fn singleton_service_instance_id(name: &str) -> ServiceInstanceId {
    ServiceInstanceId::from(service_entry_id(name))
}

/// E2E: A real `#[service]` reads pre-filled shelf data inside the sandbox.
///
/// Flow:
/// 1. `MockContext` pre-fills shelf data for "shelf_reader_service"
/// 2. `Registry` discovers the real `#[service]` by tag "sim_shelf"
/// 3. `ServiceDaemon` runs the real service
/// 4. Test verifies the service read the pre-filled value
#[tokio::test]
async fn test_real_service_reads_pre_filled_shelf() {
    let _ = service_daemon::try_init_logging();

    // Build sandbox with pre-filled shelf data.
    let shelf_reader_id = singleton_service_instance_id("shelf_reader_service");
    let (builder, handle) = MockContext::builder()
        .with_shelf::<String>(shelf_reader_id, "config_key", "hello_from_mock".into())
        .build();

    // Override registry to discover ONLY our tagged service
    let daemon = builder
        .with_registry(Registry::builder().with_tag("sim_shelf").build())
        .build();

    // Run the daemon for enough time for the service to execute
    daemon
        .run_for_duration(Duration::from_millis(500))
        .await
        .unwrap();

    // Verify: the service read the pre-filled value and wrote it to "read_result"
    assert_eq!(
        handle.get_shelf::<String>(shelf_reader_id, "read_result"),
        Some("hello_from_mock".to_string()),
        "Real service should have read and persisted the pre-filled shelf value"
    );
}

/// E2E: SimulationHandle dynamically injects shelf data while a real `#[service]` is running.
///
/// Flow:
/// 1. Sandbox starts with empty shelf
/// 2. Service runs and polls for "dynamic_key" (initially absent)
/// 3. SimulationHandle injects "dynamic_key" mid-flight
/// 4. Service observes the mutation on its next poll
#[tokio::test]
async fn test_god_hand_shelf_mutation_with_real_service() {
    let _ = service_daemon::try_init_logging();

    let shelf_reader_id = singleton_service_instance_id("shelf_reader_service");
    let (builder, handle) = MockContext::builder().build();

    let daemon = builder
        .with_registry(Registry::builder().with_tag("sim_shelf").build())
        .build();

    let cancel = daemon.cancel_token();

    // Run daemon in background
    let daemon_task = tokio::spawn(async move {
        daemon.run_for_duration(Duration::from_secs(3)).await.ok();
    });

    // Wait for the service to start
    tokio::time::sleep(Duration::from_millis(300)).await;

    // -- SimulationHandle: inject shelf data mid-flight --
    handle.set_shelf::<String>(shelf_reader_id, "dynamic_key", "injected_mid_flight".into());

    // Wait for the service to observe and persist the mutation
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify: the service saw the dynamically injected value
    assert_eq!(
        handle.get_shelf::<String>(shelf_reader_id, "dynamic_result"),
        Some("injected_mid_flight".to_string()),
        "Real service should have observed the SimulationHandle's dynamic shelf injection"
    );

    cancel.cancel();
    daemon_task.await.ok();
}

/// E2E: SimulationHandle pre-fill and mutation are observed by a real service.
///
/// This test proves the full lifecycle of simulation:
/// 1. MockContext pre-fills shelf data before startup
/// 2. Service reads pre-filled data
/// 3. SimulationHandle overwrites shelf data mid-flight
/// 4. Service observes the mutation on its next poll
#[tokio::test]
async fn test_two_phase_god_hand_with_real_service() {
    let _ = service_daemon::try_init_logging();

    // Pre-fill initial config before startup.
    let shelf_reader_id = singleton_service_instance_id("shelf_reader_service");
    let (builder, handle) = MockContext::builder()
        .with_shelf::<String>(shelf_reader_id, "config_key", "phase1_value".into())
        .build();

    let daemon = builder
        .with_registry(Registry::builder().with_tag("sim_shelf").build())
        .build();

    let cancel = daemon.cancel_token();

    let daemon_task = tokio::spawn(async move {
        daemon.run_for_duration(Duration::from_secs(3)).await.ok();
    });

    // Wait for service to read pre-filled data.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Verify the service read the pre-filled value.
    assert_eq!(
        handle.get_shelf::<String>(shelf_reader_id, "read_result"),
        Some("phase1_value".to_string()),
        "Service should have read the pre-filled 'phase1_value'"
    );

    // SimulationHandle overwrites shelf data mid-flight.
    handle.set_shelf::<String>(shelf_reader_id, "dynamic_key", "phase2_value".into());

    // Wait for service to observe the mid-flight mutation.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify the service saw the dynamically injected value.
    assert_eq!(
        handle.get_shelf::<String>(shelf_reader_id, "dynamic_result"),
        Some("phase2_value".to_string()),
        "Service should have observed the SimulationHandle's 'phase2_value'"
    );

    cancel.cancel();
    daemon_task.await.ok();
}

/// E2E: SimulationHandle flips status while a real `#[service]` is running.
///
/// Uses `service_instance_ids()` to dynamically discover visible runtime
/// instances, then flips the status via the SimulationHandle.
#[tokio::test]
async fn test_god_hand_status_flip_with_real_service() {
    let _ = service_daemon::try_init_logging();

    let status_watcher_id = singleton_service_instance_id("status_watcher_service");
    let (builder, handle) = MockContext::builder().build();

    let mut daemon = builder
        .with_registry(Registry::builder().with_tag("sim_status").build())
        .build();

    let cancel = daemon.cancel_token();

    // Run the daemon in the background -- tests should not call wait()
    // inline because it blocks the test thread waiting for OS signals.
    let daemon_task = tokio::spawn(async move {
        daemon.run().await;
        if let Err(err) = daemon.wait().await {
            panic!("daemon.wait() failed: {err}");
        }
    });

    // Wait for the runner to spawn the service and write initial status
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Discover visible ServiceInstanceIds. This includes both status_watcher_service
    // and infra services (log_service) that are auto-included.
    let ids = handle.service_instance_ids();
    assert!(
        !ids.is_empty(),
        "Should have at least one service (status_watcher_service)"
    );

    assert!(
        ids.contains(&status_watcher_id),
        "status_watcher_service should be registered"
    );

    // -- SimulationHandle: flip status to Healthy --
    handle.set_status(status_watcher_id, ServiceStatus::Healthy);

    // Wait for service to observe the new status via state()
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Verify: service persisted the observed status to shelf
    assert_eq!(
        handle.get_shelf::<String>(status_watcher_id, "observed_status"),
        Some("Healthy".to_string()),
        "Real service should have observed the SimulationHandle's status flip to Healthy"
    );

    // Graceful shutdown
    cancel.cancel();
    let _ = daemon_task.await;
}
