//! Context module — service lifecycle management infrastructure.
//!
//! This module is split into focused sub-modules:
//! - `identity`: Core data structures (`DaemonResources`, `ServiceIdentity`, task-locals)
//! - `api`: Public API functions (`done()`, `state()`, `shelve()`, `is_shutdown()`, etc.)
//! - `simulation`: `MockContext` for testing (feature-gated behind `simulation`)

// Sub-modules
pub mod api;
pub(crate) mod identity;
#[cfg(feature = "simulation")]
pub mod simulation;

// ─────────────────────────────────────────────────────────────────────────────
// Re-exports for backward compatibility
// ─────────────────────────────────────────────────────────────────────────────

// Identity types (used by runner.rs, service_daemon, macros)
// These re-exports are used by tests and by simulation_tests
#[cfg(test)]
pub(crate) use identity::{CURRENT_RESOURCES, CURRENT_SERVICE};
pub use identity::{DaemonResources, ServiceIdentity};

// Public API functions (re-exported at crate root via lib.rs)
pub use api::{
    __run_service_scope, done, is_shutdown, shelve, sleep, state, try_resolve_mock, unshelve,
    wait_shutdown,
};

// Simulation types (feature-gated)
#[cfg(feature = "simulation")]
pub use simulation::{MockContext, MockContextBuilder, SimulationOverlay};

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ServiceId, ServiceStatus};
    use std::future::Future;
    use tokio_util::sync::CancellationToken;

    fn create_test_resources() -> DaemonResources {
        DaemonResources::new()
    }

    fn create_test_identity(name: &str) -> ServiceIdentity {
        ServiceIdentity::new(
            ServiceId::new(0),
            name.to_string(),
            CancellationToken::new(),
            CancellationToken::new(),
        )
    }

    /// Helper to run a future in a service scope (for tests).
    async fn in_scope<F, Fut, T>(identity: ServiceIdentity, resources: DaemonResources, f: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        CURRENT_SERVICE
            .scope(identity, CURRENT_RESOURCES.scope(resources, f()))
            .await
    }

    #[tokio::test]
    async fn test_shelve_unshelve() {
        let resources = create_test_resources();
        let identity = create_test_identity("test_service");

        in_scope(identity, resources, || async {
            shelve("test", 42i32).await;
            let val: Option<i32> = unshelve("test").await;
            assert_eq!(val, Some(42));

            // Verify it's removed after unshelve
            let val2: Option<i32> = unshelve("test").await;
            assert_eq!(val2, None);
        })
        .await;
    }

    #[tokio::test]
    async fn test_state_transitions() {
        let resources = create_test_resources();
        let identity = create_test_identity("state_service");

        resources
            .status_plane
            .insert(ServiceId::new(0), ServiceStatus::NeedReload);

        in_scope(identity, resources, || async {
            assert!(matches!(state(), ServiceStatus::NeedReload));
        })
        .await;
    }

    #[tokio::test]
    async fn test_handshake_protocol() {
        let resources = create_test_resources();

        // Start in Initializing
        resources
            .status_plane
            .insert(ServiceId::new(0), ServiceStatus::Initializing);

        let identity = create_test_identity("handshake_service");
        let resources_clone = resources.clone();
        in_scope(identity, resources.clone(), || async move {
            // After done(), status should become Healthy
            done();
            let status = resources_clone
                .status_plane
                .get(&ServiceId::new(0))
                .map(|s| s.clone());
            assert_eq!(status, Some(ServiceStatus::Healthy));
        })
        .await;

        // Now test the descending phase
        resources
            .status_plane
            .insert(ServiceId::new(0), ServiceStatus::NeedReload);

        let identity2 = create_test_identity("handshake_service");
        let resources_clone2 = resources.clone();
        in_scope(identity2, resources.clone(), || async move {
            // After done(), status should become Terminated
            done();
            let status = resources_clone2
                .status_plane
                .get(&ServiceId::new(0))
                .map(|s| s.clone());
            assert_eq!(status, Some(ServiceStatus::Terminated));
        })
        .await;
    }

    #[tokio::test]
    async fn test_instance_isolation() {
        // This test verifies that two separate DaemonResources instances
        // do not share state, proving the removal of global pollution.
        let resources_a = create_test_resources();
        let resources_b = create_test_resources();

        resources_a
            .status_plane
            .insert(ServiceId::new(0), ServiceStatus::Healthy);
        resources_b
            .status_plane
            .insert(ServiceId::new(0), ServiceStatus::Initializing);

        let identity_a = create_test_identity("isolated_svc");
        let identity_b = create_test_identity("isolated_svc");

        let status_a = in_scope(identity_a, resources_a, || async { state() }).await;
        let status_b = in_scope(identity_b, resources_b, || async { state() }).await;

        assert_eq!(status_a, ServiceStatus::Healthy);
        assert_eq!(status_b, ServiceStatus::Initializing);
    }

    #[tokio::test]
    async fn test_is_shutdown_handshake_optimization() {
        // Verify that is_shutdown only performs the handshake once
        let resources = create_test_resources();
        resources
            .status_plane
            .insert(ServiceId::new(0), ServiceStatus::Initializing);

        let identity = create_test_identity("opt_svc");
        let resources_clone = resources.clone();

        in_scope(identity, resources, || async move {
            // First call should trigger handshake
            assert!(!is_shutdown());

            // Status should now be Healthy
            let status = resources_clone
                .status_plane
                .get(&ServiceId::new(0))
                .map(|s| s.clone());
            assert_eq!(status, Some(ServiceStatus::Healthy));

            // Revert status to Initializing to prove the flag prevents re-handshake
            resources_clone
                .status_plane
                .insert(ServiceId::new(0), ServiceStatus::Initializing);

            // Second call should NOT re-handshake (flag is set)
            assert!(!is_shutdown());

            // Status should remain Initializing because handshake was skipped
            let status2 = resources_clone
                .status_plane
                .get(&ServiceId::new(0))
                .map(|s| s.clone());
            assert_eq!(status2, Some(ServiceStatus::Initializing));
        })
        .await;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Simulation Tests (feature-gated: "simulation")
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
#[cfg(feature = "simulation")]
mod simulation_tests {
    use super::*;
    use crate::models::{ServiceId, ServiceStatus};

    #[tokio::test]
    async fn test_mock_context_shelf_injection() {
        let ctx = MockContext::builder()
            .with_service_name("shelf_test_svc")
            .with_shelf::<i32>("counter", 42i32)
            .with_status(ServiceStatus::Healthy)
            .build();

        ctx.run(|| async {
            // unshelve should return the injected value
            let val: Option<i32> = unshelve("counter").await;
            assert_eq!(val, Some(42));

            // After unshelve, the value is consumed
            let val2: Option<i32> = unshelve("counter").await;
            assert_eq!(val2, None);
        })
        .await;
    }

    #[tokio::test]
    async fn test_mock_context_status_injection() {
        let ctx = MockContext::builder()
            .with_service_name("status_test_svc")
            .with_status(ServiceStatus::Healthy)
            .build();

        ctx.run(|| async {
            assert_eq!(state(), ServiceStatus::Healthy);
        })
        .await;
    }

    #[tokio::test]
    async fn test_mock_context_provider_shadow() {
        // Test that try_resolve_mock works correctly
        let ctx = MockContext::builder()
            .with_mock::<String>("hello_mock".to_string())
            .build();

        ctx.run(|| async {
            let result = try_resolve_mock::<String>();
            assert!(result.is_some());
            assert_eq!(*result.unwrap(), "hello_mock");

            // Non-registered type should return None
            let missing = try_resolve_mock::<i64>();
            assert!(missing.is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn test_mock_context_parallel_isolation() {
        // Two parallel MockContexts with different values for the same type
        // should not interfere with each other.
        let ctx_a = MockContext::builder()
            .with_service_name("svc_a")
            .with_mock::<String>("value_a".to_string())
            .with_status(ServiceStatus::Healthy)
            .build();

        let ctx_b = MockContext::builder()
            .with_service_name("svc_b")
            .with_mock::<String>("value_b".to_string())
            .with_status(ServiceStatus::Initializing)
            .build();

        let (result_a, result_b) = tokio::join!(
            ctx_a.run(|| async {
                let mock = try_resolve_mock::<String>().unwrap();
                let status = state();
                ((*mock).clone(), status)
            }),
            ctx_b.run(|| async {
                let mock = try_resolve_mock::<String>().unwrap();
                let status = state();
                ((*mock).clone(), status)
            }),
        );

        assert_eq!(result_a.0, "value_a");
        assert_eq!(result_a.1, ServiceStatus::Healthy);
        assert_eq!(result_b.0, "value_b");
        assert_eq!(result_b.1, ServiceStatus::Initializing);
    }

    #[tokio::test]
    async fn test_mock_context_shelve_roundtrip() {
        // Test that shelve() inside MockContext writes to isolated resources
        let ctx = MockContext::builder()
            .with_service_name("roundtrip_svc")
            .with_status(ServiceStatus::Healthy)
            .build();

        ctx.run(|| async {
            shelve("key", "persisted_value".to_string()).await;
            let val: Option<String> = unshelve("key").await;
            assert_eq!(val, Some("persisted_value".to_string()));
        })
        .await;
    }

    #[tokio::test]
    async fn test_mock_context_multi_injection() {
        // Verify multiple chained injections work as expected
        let ctx = MockContext::builder()
            .with_service_name("multi_test_svc")
            .with_mock::<i32>(100)
            .with_mock::<String>("hello".to_string())
            .with_shelf::<i32>("k1", 1)
            .with_shelf::<i32>("k2", 2)
            .with_status(ServiceStatus::Healthy)
            .with_service_status(ServiceId::new(99), ServiceStatus::NeedReload)
            .build();

        ctx.run(|| async {
            // Mocks
            assert_eq!(*try_resolve_mock::<i32>().unwrap(), 100);
            assert_eq!(*try_resolve_mock::<String>().unwrap(), "hello");

            // Shelf
            assert_eq!(unshelve::<i32>("k1").await, Some(1));
            assert_eq!(unshelve::<i32>("k2").await, Some(2));

            // Status
            assert_eq!(state(), ServiceStatus::Healthy);
            let dep_status = CURRENT_RESOURCES.with(|r| {
                r.status_plane
                    .get(&ServiceId::new(99))
                    .map(|s| s.value().clone())
            });
            assert_eq!(dep_status, Some(ServiceStatus::NeedReload));
        })
        .await;
    }
}
