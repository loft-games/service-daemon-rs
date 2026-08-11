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
        DaemonInstanceId, ScalingPolicy, ServiceControl, ServiceEntry, ServiceEntryId,
        ServiceInstanceHandle, ServiceInstanceId, ServiceParam, ServiceRuntimeSnapshot,
        ServiceScheduling, ServiceStatus, TriggerRuntimeSnapshot,
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

    struct TestServiceControl {
        daemon_id: DaemonInstanceId,
        projection: Arc<crate::models::ServiceCatalogProjection>,
    }

    impl ServiceControl for TestServiceControl {
        fn daemon_id(&self) -> DaemonInstanceId {
            self.daemon_id
        }

        fn owns_service_entry(
            &self,
            entry_id: ServiceEntryId,
            entry: &'static ServiceEntry,
        ) -> bool {
            self.projection
                .resolve_entry(entry_id)
                .is_some_and(|record| std::ptr::eq(record.entry, entry))
        }

        fn service_instances_for_entry(
            &self,
            _entry_id: ServiceEntryId,
            _entry: &'static ServiceEntry,
            _control: Arc<dyn ServiceControl>,
        ) -> Vec<ServiceInstanceHandle> {
            Vec::new()
        }

        fn service_status(&self, _handle: &ServiceInstanceHandle) -> ServiceStatus {
            ServiceStatus::Terminated
        }

        fn service_runtime(
            &self,
            _handle: &ServiceInstanceHandle,
        ) -> Option<ServiceRuntimeSnapshot> {
            None
        }

        fn trigger_runtime(
            &self,
            _handle: &ServiceInstanceHandle,
        ) -> Option<TriggerRuntimeSnapshot> {
            None
        }

        fn request_stop(&self, _handle: &ServiceInstanceHandle) -> bool {
            false
        }
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

    fn resources_with_registry_tag(
        tag: &'static str,
    ) -> (Arc<DaemonResources>, Arc<dyn ServiceControl>) {
        let registry = crate::models::Registry::builder().with_tag(tag).build();
        let (_, projection, _) = registry.into_parts();
        let resources = create_test_resources();
        resources.set_service_catalog_projection(projection.clone());
        let control: Arc<dyn ServiceControl> = Arc::new(TestServiceControl {
            daemon_id: resources.daemon_id(),
            projection,
        });
        resources.set_service_control(control.clone());
        (resources, control)
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
        let (resources, _control) = resources_with_registry_tag("__handle_resolver_selected__");

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
        let (resources, _control) = resources_with_registry_tag("__handle_resolver_selected__");

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
        let (resources, _control) = resources_with_registry_tag("__handle_resolver_selected__");

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
    use crate::models::{Registry, ServiceInstanceId, ServiceStatus};

    fn selected_registry() -> Registry {
        Registry::builder()
            .with_tag("__handle_resolver_selected__")
            .build()
    }

    fn selected_instance_id(registry: &Registry) -> ServiceInstanceId {
        registry
            .services()
            .iter()
            .find(|service| service.name() == "selected_handle_test_service")
            .and_then(|service| service.instance_ids().first().copied())
            .expect("selected test service should be materialized")
    }

    #[test]
    fn test_mock_context_shelf_pre_filling() {
        // Verify that pre-filled shelf data is accessible through the handle.
        let registry = selected_registry();
        let svc_id = selected_instance_id(&registry);
        let handle = MockContext::builder()
            .with_shelf::<i32>(svc_id, "counter", 42)
            .with_shelf::<String>(svc_id, "name", "hello".to_string())
            .with_registry(registry)
            .build();
        let instance = handle
            .service_instances()
            .into_iter()
            .find(|instance| instance.name() == "selected_handle_test_service")
            .expect("selected test service should have one instance");

        assert_eq!(handle.get_shelf::<i32>(&instance, "counter"), Some(42));
        assert_eq!(
            handle.get_shelf::<String>(&instance, "name"),
            Some("hello".to_string())
        );

        assert_eq!(handle.daemon().id(), handle.id());
    }

    #[test]
    fn test_mock_context_status_pre_filling() {
        let registry = selected_registry();
        let svc_id = selected_instance_id(&registry);
        let handle = MockContext::builder()
            .with_status(svc_id, ServiceStatus::Healthy)
            .with_registry(registry)
            .build();
        let instance = handle
            .service_instances()
            .into_iter()
            .find(|instance| instance.name() == "selected_handle_test_service")
            .expect("selected test service should have one instance");

        assert_eq!(handle.get_status(&instance), Some(ServiceStatus::Healthy));
    }

    #[test]
    fn test_simulation_handle_dynamic_shelf_update() {
        let handle = MockContext::builder()
            .with_registry(selected_registry())
            .build();
        let instance = handle
            .service_instances()
            .into_iter()
            .find(|instance| instance.name() == "selected_handle_test_service")
            .expect("selected test service should have one instance");

        assert!(!handle.has_shelf(&instance, "counter"));

        assert!(handle.set_shelf::<i32>(&instance, "counter", 99));

        assert_eq!(handle.get_shelf::<i32>(&instance, "counter"), Some(99));
    }

    #[test]
    fn test_simulation_handle_dynamic_status_update() {
        let registry = selected_registry();
        let svc_id = selected_instance_id(&registry);
        let handle = MockContext::builder()
            .with_status(svc_id, ServiceStatus::Initializing)
            .with_registry(registry)
            .build();
        let instance = handle
            .service_instances()
            .into_iter()
            .find(|instance| instance.name() == "selected_handle_test_service")
            .expect("selected test service should have one instance");

        assert_eq!(
            handle.get_status(&instance),
            Some(ServiceStatus::Initializing)
        );

        assert!(handle.set_status(&instance, ServiceStatus::NeedReload));

        assert_eq!(
            handle.get_status(&instance),
            Some(ServiceStatus::NeedReload)
        );
    }

    #[test]
    fn test_mock_context_isolation() {
        // Two MockContexts should have completely separate resources.
        let registry_a = selected_registry();
        let registry_b = selected_registry();
        let svc_id_a = selected_instance_id(&registry_a);
        let svc_id_b = selected_instance_id(&registry_b);
        let handle_a = MockContext::builder()
            .with_status(svc_id_a, ServiceStatus::Healthy)
            .with_registry(registry_a)
            .build();
        let handle_b = MockContext::builder()
            .with_status(svc_id_b, ServiceStatus::Initializing)
            .with_registry(registry_b)
            .build();
        let instance_a = handle_a
            .service_instances()
            .into_iter()
            .find(|instance| instance.name() == "selected_handle_test_service")
            .expect("selected test service should have one instance");
        let instance_b = handle_b
            .service_instances()
            .into_iter()
            .find(|instance| instance.name() == "selected_handle_test_service")
            .expect("selected test service should have one instance");

        assert_eq!(
            handle_a.get_status(&instance_a),
            Some(ServiceStatus::Healthy)
        );
        assert_eq!(
            handle_b.get_status(&instance_b),
            Some(ServiceStatus::Initializing)
        );

        assert!(handle_a.set_status(&instance_a, ServiceStatus::Terminated));
        assert_eq!(
            handle_b.get_status(&instance_b),
            Some(ServiceStatus::Initializing)
        );
    }
}
