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

// Identity types (used by runner modules, service_daemon, macros)
// These re-exports are used by tests and by simulation_tests
pub(crate) use identity::process_token;
#[cfg(test)]
pub(crate) use identity::{CURRENT_RESOURCES, CURRENT_SERVICE};
pub use identity::{DaemonResources, ServiceIdentity};

// Public API functions (re-exported at crate root via lib.rs)
#[doc(hidden)]
pub use api::__resolve_service_handle;
pub(crate) use api::{__run_daemon_resources_scope, __run_daemon_resources_sync_scope};
pub use api::{
    __run_service_scope, current_cancellation_token, current_service_instance_id, done,
    is_shutdown, shelve, shelve_clone, sleep, spawn_with_context, state, trigger_config, unshelve,
    wait_shutdown,
};
pub(crate) use api::{
    clear_trigger_policy_overlay, current_daemon_diagnostics, current_generation_diagnostics,
    current_service_generation, current_trigger_pressure, effective_trigger_policy,
    register_current_trigger_policy_overlay, register_current_trigger_runtime,
    request_trigger_policy_overlay,
};

#[cfg(feature = "simulation")]
pub use simulation::{MockContext, MockContextBuilder, SimulationHandle};

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        ScalingPolicy, ServiceEntry, ServiceInstanceId, ServiceParam, ServiceScheduling,
        ServiceStatus,
    };
    use futures::future::BoxFuture;
    use linkme::distributed_slice;
    use std::any::TypeId;
    use std::future::Future;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    fn create_test_resources() -> Arc<DaemonResources> {
        DaemonResources::new()
    }

    fn create_test_identity(name: &'static str) -> ServiceIdentity {
        ServiceIdentity::new(
            ServiceInstanceId::new(uuid::Uuid::from_u128(0)),
            name,
            CancellationToken::new(),
            CancellationToken::new(),
        )
    }

    fn selected_handle_test_wrapper(
        _: CancellationToken,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn excluded_handle_test_wrapper(
        _: CancellationToken,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn unlinked_handle_test_wrapper(
        _: CancellationToken,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    #[allow(unsafe_code)]
    #[distributed_slice(crate::models::SERVICE_REGISTRY)]
    static SELECTED_HANDLE_TEST_ENTRY: ServiceEntry = ServiceEntry {
        name: "selected_handle_test_service",
        module: "core::context::tests",
        params: &[] as &[ServiceParam],
        wrapper: selected_handle_test_wrapper,
        watcher: None,
        priority: 50,
        scheduling: ServiceScheduling::Standard,
        tags: &["__handle_resolver_selected__"],
    };

    #[allow(unsafe_code)]
    #[distributed_slice(crate::models::SERVICE_REGISTRY)]
    static EXCLUDED_HANDLE_TEST_ENTRY: ServiceEntry = ServiceEntry {
        name: "excluded_handle_test_service",
        module: "core::context::tests",
        params: &[] as &[ServiceParam],
        wrapper: excluded_handle_test_wrapper,
        watcher: None,
        priority: 50,
        scheduling: ServiceScheduling::Standard,
        tags: &["__handle_resolver_excluded__"],
    };

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

    fn resources_with_registry_tag(tag: &'static str) -> Arc<DaemonResources> {
        let registry = crate::models::Registry::builder().with_tag(tag).build();
        let (_, projection) = registry.into_parts();
        let resources = create_test_resources();
        resources.set_service_catalog_projection(projection);
        resources
    }

    #[test]
    fn service_handle_resolution_fails_outside_daemon_scope() {
        let error = __resolve_service_handle(selected_handle_test_wrapper).unwrap_err();

        assert!(
            matches!(error, crate::ProviderError::Fatal(message) if message.contains("requires a daemon provider scope"))
        );
    }

    #[tokio::test]
    async fn service_handle_resolution_returns_daemon_local_handle() {
        let resources = resources_with_registry_tag("__handle_resolver_selected__");

        let handle = __run_daemon_resources_sync_scope(resources, || {
            __resolve_service_handle(selected_handle_test_wrapper)
        })
        .await
        .expect("selected service should resolve to handle");

        assert_eq!(handle.name(), "selected_handle_test_service");
        assert_eq!(handle.module(), "core::context::tests");
    }

    #[tokio::test]
    async fn service_handle_resolution_reports_unlinked_target() {
        let resources = resources_with_registry_tag("__handle_resolver_selected__");

        let error = __run_daemon_resources_sync_scope(resources, || {
            __resolve_service_handle(unlinked_handle_test_wrapper)
        })
        .await
        .unwrap_err();

        assert!(
            matches!(error, crate::ProviderError::Fatal(message) if message.contains("not linked into SERVICE_REGISTRY"))
        );
    }

    #[tokio::test]
    async fn service_handle_resolution_reports_linked_but_not_selected_target() {
        let resources = resources_with_registry_tag("__handle_resolver_selected__");

        let error = __run_daemon_resources_sync_scope(resources, || {
            __resolve_service_handle(excluded_handle_test_wrapper)
        })
        .await
        .unwrap_err();

        assert!(
            matches!(error, crate::ProviderError::Fatal(message) if message.contains("linked but not selected"))
        );
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
    async fn test_shelf_isolated_by_service_instance_id_for_duplicate_names() {
        let resources = create_test_resources();
        let first = ServiceIdentity::new(
            ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            "duplicate_name",
            CancellationToken::new(),
            CancellationToken::new(),
        );
        let second = ServiceIdentity::new(
            ServiceInstanceId::new(uuid::Uuid::from_u128(2)),
            "duplicate_name",
            CancellationToken::new(),
            CancellationToken::new(),
        );

        in_scope(first, resources.clone(), || async {
            shelve("value", 42i32).await;
        })
        .await;

        in_scope(second, resources.clone(), || async {
            let val: Option<i32> = unshelve("value").await;
            assert_eq!(val, None);
            shelve("value", 7i32).await;
        })
        .await;

        in_scope(
            ServiceIdentity::new(
                ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
                "duplicate_name",
                CancellationToken::new(),
                CancellationToken::new(),
            ),
            resources,
            || async {
                let val: Option<i32> = unshelve("value").await;
                assert_eq!(val, Some(42));
            },
        )
        .await;
    }

    #[tokio::test]
    async fn test_state_transitions() {
        let resources = create_test_resources();
        let identity = create_test_identity("state_service");

        resources.status_plane.insert(
            ServiceInstanceId::new(uuid::Uuid::from_u128(0)),
            ServiceStatus::NeedReload,
        );

        in_scope(identity, resources, || async {
            assert!(matches!(state(), ServiceStatus::NeedReload));
        })
        .await;
    }

    #[tokio::test]
    async fn test_handshake_protocol() {
        let resources = create_test_resources();

        // Start in Initializing
        resources.status_plane.insert(
            ServiceInstanceId::new(uuid::Uuid::from_u128(0)),
            ServiceStatus::Initializing,
        );

        let identity = create_test_identity("handshake_service");
        let resources_clone = resources.clone();
        in_scope(identity, resources.clone(), || async move {
            // After done(), status should become Healthy
            done();
            let status = resources_clone
                .status_plane
                .get(&ServiceInstanceId::new(uuid::Uuid::from_u128(0)))
                .map(|s| s.clone());
            assert_eq!(status, Some(ServiceStatus::Healthy));
        })
        .await;

        // Now test the descending phase
        resources.status_plane.insert(
            ServiceInstanceId::new(uuid::Uuid::from_u128(0)),
            ServiceStatus::NeedReload,
        );

        let identity2 = create_test_identity("handshake_service");
        let resources_clone2 = resources.clone();
        in_scope(identity2, resources.clone(), || async move {
            // After done(), status should become Terminated
            done();
            let status = resources_clone2
                .status_plane
                .get(&ServiceInstanceId::new(uuid::Uuid::from_u128(0)))
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

        resources_a.status_plane.insert(
            ServiceInstanceId::new(uuid::Uuid::from_u128(0)),
            ServiceStatus::Healthy,
        );
        resources_b.status_plane.insert(
            ServiceInstanceId::new(uuid::Uuid::from_u128(0)),
            ServiceStatus::Initializing,
        );

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
        resources.status_plane.insert(
            ServiceInstanceId::new(uuid::Uuid::from_u128(0)),
            ServiceStatus::Initializing,
        );

        let identity = create_test_identity("opt_svc");
        let resources_clone = resources.clone();

        in_scope(identity, resources, || async move {
            // First call should trigger handshake
            assert!(!is_shutdown());

            // Status should now be Healthy
            let status = resources_clone
                .status_plane
                .get(&ServiceInstanceId::new(uuid::Uuid::from_u128(0)))
                .map(|s| s.clone());
            assert_eq!(status, Some(ServiceStatus::Healthy));

            // Revert status to Initializing to prove the flag prevents re-handshake
            resources_clone.status_plane.insert(
                ServiceInstanceId::new(uuid::Uuid::from_u128(0)),
                ServiceStatus::Initializing,
            );

            // Second call should NOT re-handshake (flag is set)
            assert!(!is_shutdown());

            // Status should remain Initializing because handshake was skipped
            let status2 = resources_clone
                .status_plane
                .get(&ServiceInstanceId::new(uuid::Uuid::from_u128(0)))
                .map(|s| s.clone());
            assert_eq!(status2, Some(ServiceStatus::Initializing));
        })
        .await;
    }

    #[tokio::test]
    async fn test_current_cancellation_token_uses_service_child_token_in_scope() {
        let resources = create_test_resources();
        let identity = create_test_identity("cancel_scope");
        let parent = identity.cancellation_token.clone();

        in_scope(identity, resources, || async move {
            let child = current_cancellation_token();
            assert!(!child.is_cancelled());
            parent.cancel();
            child.cancelled().await;
            assert!(child.is_cancelled());
        })
        .await;
    }

    #[tokio::test]
    async fn test_current_cancellation_token_is_standalone_outside_scope() {
        let first = current_cancellation_token();
        let second = current_cancellation_token();

        assert!(!first.is_cancelled());
        assert!(!second.is_cancelled());

        first.cancel();

        assert!(first.is_cancelled());
        assert!(
            !second.is_cancelled(),
            "outside service scope the fallback token should stay independent"
        );
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
            trigger_config::<ScalingPolicy>()
        })
        .await;

        assert!(result.is_none());
    }

    /// Verify that trigger_config returns the registered config.
    #[tokio::test]
    async fn test_trigger_config_returns_registered_value() {
        let resources = create_test_resources();
        let sp = ScalingPolicy::builder()
            .initial_concurrency(8)
            .max_concurrency(32)
            .build();
        resources
            .trigger_configs
            .insert(TypeId::of::<ScalingPolicy>(), Box::new(sp));

        let identity = create_test_identity("tc_registered");

        let result = in_scope(identity, resources, || async {
            trigger_config::<ScalingPolicy>()
        })
        .await;

        let fetched = result.expect("should return Some");
        assert_eq!(fetched.initial_concurrency(), 8);
        assert_eq!(fetched.max_concurrency(), 32);
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
        let sp = ScalingPolicy::builder().initial_concurrency(4).build();
        let custom = MyCustomConfig { rate_limit: 100 };
        resources
            .trigger_configs
            .insert(TypeId::of::<ScalingPolicy>(), Box::new(sp));
        resources
            .trigger_configs
            .insert(TypeId::of::<MyCustomConfig>(), Box::new(custom));

        let identity = create_test_identity("tc_multi");

        in_scope(identity, resources, || async {
            let sp = trigger_config::<ScalingPolicy>().expect("ScalingPolicy should be present");
            assert_eq!(sp.initial_concurrency(), 4);

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
        let result = trigger_config::<ScalingPolicy>();
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
    use crate::models::{ServiceInstanceId, ServiceStatus};

    #[test]
    fn test_mock_context_shelf_pre_filling() {
        // Verify that pre-filled shelf data is accessible through the handle.
        let svc_id = ServiceInstanceId::new(uuid::Uuid::from_u128(7));
        let (builder, handle) = MockContext::builder()
            .with_shelf::<i32>(svc_id, "counter", 42)
            .with_shelf::<String>(svc_id, "name", "hello".to_string())
            .build();

        assert_eq!(handle.get_shelf::<i32>(svc_id, "counter"), Some(42));
        assert_eq!(
            handle.get_shelf::<String>(svc_id, "name"),
            Some("hello".to_string())
        );

        // Builder should be valid (not consumed)
        let _ = builder;
    }

    #[test]
    fn test_mock_context_status_pre_filling() {
        let svc_id = ServiceInstanceId::new(uuid::Uuid::from_u128(1));
        let (_, handle) = MockContext::builder()
            .with_status(svc_id, ServiceStatus::Healthy)
            .build();

        assert_eq!(handle.get_status(svc_id), Some(ServiceStatus::Healthy));
    }

    #[test]
    fn test_simulation_handle_dynamic_shelf_update() {
        let (_, handle) = MockContext::builder().build();
        let svc_id = ServiceInstanceId::new(uuid::Uuid::from_u128(7));

        assert!(!handle.has_shelf(svc_id, "counter"));

        handle.set_shelf::<i32>(svc_id, "counter", 99);

        assert_eq!(handle.get_shelf::<i32>(svc_id, "counter"), Some(99));
    }

    #[test]
    fn test_simulation_handle_dynamic_status_update() {
        let svc_id = ServiceInstanceId::new(uuid::Uuid::from_u128(42));
        let (_, handle) = MockContext::builder()
            .with_status(svc_id, ServiceStatus::Initializing)
            .build();

        assert_eq!(handle.get_status(svc_id), Some(ServiceStatus::Initializing));

        handle.set_status(svc_id, ServiceStatus::NeedReload);

        assert_eq!(handle.get_status(svc_id), Some(ServiceStatus::NeedReload));
    }

    #[test]
    fn test_mock_context_isolation() {
        // Two MockContexts should have completely separate resources.
        let (_, handle_a) = MockContext::builder()
            .with_status(
                ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
                ServiceStatus::Healthy,
            )
            .build();
        let (_, handle_b) = MockContext::builder()
            .with_status(
                ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
                ServiceStatus::Initializing,
            )
            .build();

        assert_eq!(
            handle_a.get_status(ServiceInstanceId::new(uuid::Uuid::from_u128(1))),
            Some(ServiceStatus::Healthy)
        );
        assert_eq!(
            handle_b.get_status(ServiceInstanceId::new(uuid::Uuid::from_u128(1))),
            Some(ServiceStatus::Initializing)
        );

        handle_a.set_status(
            ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            ServiceStatus::Terminated,
        );
        assert_eq!(
            handle_b.get_status(ServiceInstanceId::new(uuid::Uuid::from_u128(1))),
            Some(ServiceStatus::Initializing)
        );
    }
}
