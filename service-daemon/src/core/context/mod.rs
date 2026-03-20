//! Context module -- service lifecycle management infrastructure.
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

// -----------------------------------------------------------------------------
// Re-exports for backward compatibility
// -----------------------------------------------------------------------------

// Identity types (used by runner.rs, service_daemon, macros)
// These re-exports are used by tests and by simulation_tests
#[cfg(test)]
pub(crate) use identity::{CURRENT_RESOURCES, CURRENT_SERVICE};
pub use identity::{DaemonResources, ServiceIdentity};
pub(crate) use identity::process_token;

// Public API functions (re-exported at crate root via lib.rs)
pub use api::{
    __run_service_scope, done, is_shutdown, shelve, shelve_clone, sleep, state, trigger_config,
    unshelve, wait_shutdown,
};

#[cfg(feature = "simulation")]
pub use simulation::{MockContext, MockContextBuilder, SimulationHandle};

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ServiceId, ServiceStatus};
    use std::any::TypeId;
    use std::future::Future;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    fn create_test_resources() -> Arc<DaemonResources> {
        DaemonResources::new()
    }

    fn create_test_identity(name: &'static str) -> ServiceIdentity {
        ServiceIdentity::new(
            ServiceId::new(0),
            name,
            CancellationToken::new(),
            CancellationToken::new(),
        )
    }

    /// Helper to run a future in a service scope (for tests).
    async fn in_scope<F, Fut, T>(
        identity: ServiceIdentity,
        resources: Arc<DaemonResources>,
        f: F,
    ) -> T
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

    // -----------------------------------------------------------------------
    // New tests: trigger_config registry
    // -----------------------------------------------------------------------

    /// Verify that trigger_config returns None when no config is registered.
    #[tokio::test]
    async fn test_trigger_config_returns_none_when_empty() {
        let resources = create_test_resources();
        let identity = create_test_identity("tc_empty");

        let result = in_scope(identity, resources, || async {
            trigger_config::<crate::models::ScalingPolicy>()
        })
        .await;

        assert!(result.is_none());
    }

    /// Verify that trigger_config returns the registered config.
    #[tokio::test]
    async fn test_trigger_config_returns_registered_value() {
        let resources = create_test_resources();
        let sp = crate::models::ScalingPolicy::builder()
            .initial_concurrency(8)
            .max_concurrency(32)
            .build();
        resources
            .trigger_configs
            .insert(TypeId::of::<crate::models::ScalingPolicy>(), Box::new(sp));

        let identity = create_test_identity("tc_registered");

        let result = in_scope(identity, resources, || async {
            trigger_config::<crate::models::ScalingPolicy>()
        })
        .await;

        let fetched = result.expect("should return Some");
        assert_eq!(fetched.initial_concurrency, 8);
        assert_eq!(fetched.max_concurrency, 32);
    }

    /// Verify that multiple config types are independently stored and retrieved.
    #[tokio::test]
    async fn test_trigger_config_multiple_types_isolation() {
        #[derive(Debug, Clone, PartialEq)]
        struct MyCustomConfig {
            rate_limit: u32,
        }

        let resources = create_test_resources();

        // Register two different types
        let sp = crate::models::ScalingPolicy::builder()
            .initial_concurrency(4)
            .build();
        let custom = MyCustomConfig { rate_limit: 100 };
        resources
            .trigger_configs
            .insert(TypeId::of::<crate::models::ScalingPolicy>(), Box::new(sp));
        resources
            .trigger_configs
            .insert(TypeId::of::<MyCustomConfig>(), Box::new(custom));

        let identity = create_test_identity("tc_multi");

        in_scope(identity, resources, || async {
            let sp = trigger_config::<crate::models::ScalingPolicy>()
                .expect("ScalingPolicy should be present");
            assert_eq!(sp.initial_concurrency, 4);

            let custom =
                trigger_config::<MyCustomConfig>().expect("MyCustomConfig should be present");
            assert_eq!(custom.rate_limit, 100);

            // Unregistered type should return None
            let missing = trigger_config::<String>();
            assert!(missing.is_none());
        })
        .await;
    }

    /// Verify that trigger_config returns None outside a service scope.
    #[tokio::test]
    async fn test_trigger_config_outside_scope_returns_none() {
        let result = trigger_config::<crate::models::ScalingPolicy>();
        assert!(result.is_none());
    }
}

// -----------------------------------------------------------------------------
// Simulation Tests (feature-gated: "simulation")
// -----------------------------------------------------------------------------
#[cfg(test)]
#[cfg(feature = "simulation")]
mod simulation_tests {
    use crate::MockContext;
    use crate::models::{ServiceId, ServiceStatus};

    #[test]
    fn test_mock_context_shelf_pre_filling() {
        // Verify that pre-filled shelf data is accessible through the handle.
        let (builder, handle) = MockContext::builder()
            .with_shelf::<i32>("test_svc", "counter", 42)
            .with_shelf::<String>("test_svc", "name", "hello".to_string())
            .build();

        // The handle should see the pre-filled resources
        let resources = handle.resources();
        let shelf = resources.shelf.get("test_svc").unwrap();
        let counter = shelf.get("counter").unwrap();
        assert_eq!(counter.value().downcast_ref::<i32>(), Some(&42));

        // Builder should be valid (not consumed)
        let _ = builder;
    }

    #[test]
    fn test_mock_context_status_pre_filling() {
        let svc_id = ServiceId::new(1);
        let (_, handle) = MockContext::builder()
            .with_status(svc_id, ServiceStatus::Healthy)
            .build();

        let resources = handle.resources();
        let status = resources.status_plane.get(&svc_id).unwrap().clone();
        assert_eq!(status, ServiceStatus::Healthy);
    }

    #[test]
    fn test_simulation_handle_dynamic_shelf_update() {
        let (_, handle) = MockContext::builder().build();

        // Initially empty
        assert!(handle.resources().shelf.get("svc").is_none());

        // Dynamic injection via SimulationHandle
        handle.set_shelf::<i32>("svc", "counter", 99);

        // Now visible
        let resources = handle.resources();
        let shelf = resources.shelf.get("svc").unwrap();
        let val = shelf.get("counter").unwrap();
        assert_eq!(val.value().downcast_ref::<i32>(), Some(&99));
    }

    #[test]
    fn test_simulation_handle_dynamic_status_update() {
        let svc_id = ServiceId::new(42);
        let (_, handle) = MockContext::builder()
            .with_status(svc_id, ServiceStatus::Initializing)
            .build();

        // Phase 1: initial state
        assert_eq!(
            handle
                .resources()
                .status_plane
                .get(&svc_id)
                .unwrap()
                .clone(),
            ServiceStatus::Initializing
        );

        // Phase 2: SimulationHandle flips status
        handle.set_status(svc_id, ServiceStatus::NeedReload);

        assert_eq!(
            handle
                .resources()
                .status_plane
                .get(&svc_id)
                .unwrap()
                .clone(),
            ServiceStatus::NeedReload
        );
    }

    #[test]
    fn test_mock_context_isolation() {
        // Two MockContexts should have completely separate resources.
        let (_, handle_a) = MockContext::builder()
            .with_status(ServiceId::new(1), ServiceStatus::Healthy)
            .build();
        let (_, handle_b) = MockContext::builder()
            .with_status(ServiceId::new(1), ServiceStatus::Initializing)
            .build();

        assert_eq!(
            handle_a
                .resources()
                .status_plane
                .get(&ServiceId::new(1))
                .unwrap()
                .clone(),
            ServiceStatus::Healthy
        );
        assert_eq!(
            handle_b
                .resources()
                .status_plane
                .get(&ServiceId::new(1))
                .unwrap()
                .clone(),
            ServiceStatus::Initializing
        );

        // Mutation in A should NOT affect B
        handle_a.set_status(ServiceId::new(1), ServiceStatus::Terminated);
        assert_eq!(
            handle_b
                .resources()
                .status_plane
                .get(&ServiceId::new(1))
                .unwrap()
                .clone(),
            ServiceStatus::Initializing
        );
    }
}
