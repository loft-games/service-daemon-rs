//! # Simulation Example — Interactive Debugging Sandbox
//!
//! This example demonstrates the `simulation` feature with **real `#[service]`** functions:
//! - `MockContext::builder()` for creating isolated simulation environments
//! - `SimulationHandle` for dynamic "God Hand" intervention
//! - Real `#[service]`-annotated services running inside a sandbox `ServiceDaemon`
//!
//! The `simulation` feature is compile-time gated: all simulation types are
//! physically absent from production builds.
//!
//! **Run tests**: `cargo test -p example-simulation`

use service_daemon::{service, shelve, state, unshelve};

/// This file is intentionally minimal — the real demonstration is in the tests below.
fn main() {
    tracing_subscriber::fmt::init();
    tracing::info!("This example is designed to be run as tests:");
    tracing::info!("  cargo test -p example-simulation");
}

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
async fn shelf_reader_service() -> anyhow::Result<()> {
    tracing::info!("shelf_reader_service started, status = {:?}", state());

    // Try to read pre-filled shelf data (injected by MockContext)
    let config: Option<String> = unshelve("config_key").await;
    tracing::info!("shelf_reader_service: unshelved config_key = {:?}", config);

    if let Some(val) = config {
        // Signal to the test that we successfully read the value.
        // We persist it back to the shelf under a "result" key for the test to verify.
        shelve("read_result", val).await;
    }

    // Keep running until shutdown, periodically checking for God Hand mutations.
    loop {
        if service_daemon::is_shutdown() {
            break;
        }

        // Check for dynamically injected values (God Hand phase 2)
        let dynamic_val: Option<String> = unshelve("dynamic_key").await;
        if let Some(val) = dynamic_val {
            tracing::info!(
                "shelf_reader_service: God Hand injected dynamic_key = {:?}",
                val
            );
            shelve("dynamic_result", val).await;
        }

        service_daemon::sleep(std::time::Duration::from_millis(50)).await;
    }

    Ok(())
}

/// A real service that monitors its own lifecycle status via `state()`.
///
/// Tagged with "sim_status" for selective inclusion in simulation tests.
#[service(tags = ["sim_status"])]
async fn status_watcher_service() -> anyhow::Result<()> {
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

        service_daemon::sleep(std::time::Duration::from_millis(50)).await;
    }

    Ok(())
}

// =============================================================================
// Integration Tests
// =============================================================================
#[cfg(test)]
mod tests {
    use service_daemon::{MockContext, Registry};
    use std::time::Duration;

    /// E2E: A real `#[service]` reads pre-filled shelf data inside the sandbox.
    ///
    /// Flow:
    /// 1. `MockContext` pre-fills shelf data for "shelf_reader_service"
    /// 2. `Registry` discovers the real `#[service]` by tag "sim_shelf"
    /// 3. `ServiceDaemon` runs the real service
    /// 4. Test verifies the service read the pre-filled value
    #[tokio::test]
    async fn test_real_service_reads_pre_filled_shelf() {
        let _ = tracing_subscriber::fmt::try_init();

        // Phase 1: Build sandbox with pre-filled shelf data
        let (builder, handle) = MockContext::builder()
            .with_shelf::<String>(
                "shelf_reader_service",
                "config_key",
                "hello_from_mock".into(),
            )
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
        let resources = handle.resources();
        let shelf = resources.shelf.get("shelf_reader_service").unwrap();
        let result = shelf.get("read_result").unwrap();
        assert_eq!(
            result.value().downcast_ref::<String>(),
            Some(&"hello_from_mock".to_string()),
            "Real service should have read and persisted the pre-filled shelf value"
        );
    }

    /// E2E: God Hand dynamically injects shelf data while a real `#[service]` is running.
    ///
    /// Flow:
    /// 1. Sandbox starts with empty shelf
    /// 2. Service runs and polls for "dynamic_key" (initially absent)
    /// 3. God Hand injects "dynamic_key" mid-flight
    /// 4. Service observes the mutation on its next poll
    #[tokio::test]
    async fn test_god_hand_shelf_mutation_with_real_service() {
        let _ = tracing_subscriber::fmt::try_init();

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

        // ── God Hand: inject shelf data mid-flight ──
        handle.set_shelf::<String>(
            "shelf_reader_service",
            "dynamic_key",
            "injected_mid_flight".into(),
        );

        // Wait for the service to observe and persist the mutation
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Verify: the service saw the dynamically injected value
        let resources = handle.resources();
        let shelf = resources.shelf.get("shelf_reader_service").unwrap();
        let result = shelf.get("dynamic_result").unwrap();
        assert_eq!(
            result.value().downcast_ref::<String>(),
            Some(&"injected_mid_flight".to_string()),
            "Real service should have observed the God Hand's dynamic shelf injection"
        );

        cancel.cancel();
        daemon_task.await.ok();
    }

    /// E2E: Two-phase God Hand — pre-fill then mutate, observed by a real service.
    ///
    /// This test proves the full lifecycle of simulation:
    /// 1. MockContext pre-fills shelf data (Phase 1)
    /// 2. Service reads pre-filled data
    /// 3. God Hand overwrites shelf data mid-flight (Phase 2)
    /// 4. Service observes the mutation on its next poll
    #[tokio::test]
    async fn test_two_phase_god_hand_with_real_service() {
        let _ = tracing_subscriber::fmt::try_init();

        // Phase 1: pre-fill initial config
        let (builder, handle) = MockContext::builder()
            .with_shelf::<String>("shelf_reader_service", "config_key", "phase1_value".into())
            .build();

        let daemon = builder
            .with_registry(Registry::builder().with_tag("sim_shelf").build())
            .build();

        let cancel = daemon.cancel_token();

        let daemon_task = tokio::spawn(async move {
            daemon.run_for_duration(Duration::from_secs(3)).await.ok();
        });

        // Wait for service to read Phase 1 data
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Verify Phase 1: service read the pre-filled value
        {
            let resources = handle.resources();
            let shelf = resources.shelf.get("shelf_reader_service").unwrap();
            let result = shelf.get("read_result").unwrap();
            assert_eq!(
                result.value().downcast_ref::<String>(),
                Some(&"phase1_value".to_string()),
                "Phase 1: Service should have read the pre-filled 'phase1_value'"
            );
        }

        // ── Phase 2: God Hand overwrites shelf data mid-flight ──
        handle.set_shelf::<String>("shelf_reader_service", "dynamic_key", "phase2_value".into());

        // Wait for service to observe Phase 2
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Verify Phase 2: service saw the dynamically injected value
        {
            let resources = handle.resources();
            let shelf = resources.shelf.get("shelf_reader_service").unwrap();
            let result = shelf.get("dynamic_result").unwrap();
            assert_eq!(
                result.value().downcast_ref::<String>(),
                Some(&"phase2_value".to_string()),
                "Phase 2: Service should have observed the God Hand's 'phase2_value'"
            );
        }

        cancel.cancel();
        daemon_task.await.ok();
    }

    /// E2E: God Hand flips status while a real `#[service]` is running.
    ///
    /// Uses `service_ids()` to dynamically discover the `ServiceId`
    /// assigned by `Registry`, then flips the status via the God Hand.
    #[tokio::test]
    async fn test_god_hand_status_flip_with_real_service() {
        let _ = tracing_subscriber::fmt::try_init();

        let (builder, handle) = MockContext::builder().build();

        let daemon = builder
            .with_registry(Registry::builder().with_tag("sim_status").build())
            .build();

        let _cancel = daemon.cancel_token();

        // Use daemon.run() which correctly wires cancellation tokens
        let daemon_task = tokio::spawn(async move {
            daemon.run().await.ok();
        });

        // Wait for the runner to spawn the service and write initial status
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Discover the real ServiceId via the new API
        let ids = handle.service_ids();
        assert_eq!(
            ids.len(),
            1,
            "Should have exactly one service (status_watcher_service)"
        );
        let svc_id = ids[0];

        // ── God Hand: flip status to Healthy ──
        handle.set_status(svc_id, service_daemon::ServiceStatus::Healthy);

        // Wait for service to observe the new status via state()
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Verify: service persisted the observed status to shelf
        let resources = handle.resources();
        let shelf = resources.shelf.get("status_watcher_service").unwrap();
        let status_str = shelf.get("observed_status").unwrap();
        assert_eq!(
            status_str.value().downcast_ref::<String>(),
            Some(&"Healthy".to_string()),
            "Real service should have observed the God Hand's status flip to Healthy"
        );

        // Cleanup: abort directly since graceful shutdown is not under test here
        daemon_task.abort();
        let _ = daemon_task.await;
    }
}
