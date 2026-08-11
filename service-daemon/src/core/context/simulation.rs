//! Test-only context helpers for running a real `ServiceDaemon` with injected
//! shelf, status, and provider resources.
//!
//! This module is gated behind the `simulation` feature flag and is removed
//! from production builds.

use crate::core::context::identity::DaemonResources;
use crate::core::service_daemon::{DaemonInstanceHandle, RestartPolicy, ServiceDaemonBuilder};
use crate::models::{
    DaemonInstanceId, Registry, Result as ServiceResult, ServiceHandle, ServiceInstanceHandle,
    ServiceInstanceId, ServiceStatus,
};

use std::any::Any;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// A handle for controlling and inspecting one simulation daemon.
///
/// `SimulationHandle` is the public simulation API surface. It delegates daemon
/// lifecycle work to the underlying [`DaemonInstanceHandle`] and sends
/// simulation-only mutations through daemon-owned hooks.
///
/// # Example
/// ```rust,ignore
/// let simulation = MockContext::builder()
///     .with_registry(Registry::builder().with_tag("test").build())
///     .build();
///
/// simulation.run().await;
/// let instance = simulation.service_instances()[0].clone();
/// simulation.set_shelf::<String>(&instance, "db_url", "new://host".into());
/// simulation.set_status(&instance, ServiceStatus::NeedReload);
/// ```
#[derive(Clone)]
pub struct SimulationHandle {
    daemon: DaemonInstanceHandle,
}

impl SimulationHandle {
    /// Creates a new `SimulationHandle` wrapping the given daemon.
    pub(crate) fn new(daemon: DaemonInstanceHandle) -> Self {
        Self { daemon }
    }

    /// Returns the daemon instance controlled by this simulation.
    pub fn daemon(&self) -> &DaemonInstanceHandle {
        &self.daemon
    }

    /// Return this simulation daemon identity.
    pub fn id(&self) -> DaemonInstanceId {
        self.daemon.id()
    }

    /// Get the cancellation token for this simulation daemon.
    pub fn cancel_token(&self) -> CancellationToken {
        self.daemon.cancel_token()
    }

    /// Start the simulation daemon in the background.
    pub async fn run(&self) {
        self.daemon.run().await;
    }

    /// Wait for the simulation daemon to stop.
    pub async fn wait(&self) -> ServiceResult<()> {
        self.daemon.wait().await
    }

    /// Trigger graceful shutdown of the simulation daemon.
    pub fn shutdown(&self) {
        self.daemon.shutdown();
    }

    /// Run the simulation daemon for a limited duration and then stop services.
    pub async fn run_for_duration(&self, duration: Duration) -> ServiceResult<()> {
        self.daemon.simulation_run_for_duration(duration).await
    }

    /// Return all service instances owned by this simulation daemon.
    pub fn service_instances(&self) -> Vec<ServiceInstanceHandle> {
        self.daemon.service_instances()
    }

    /// Return service instances for one service definition selected by this daemon.
    pub fn service_instances_for(&self, handle: &ServiceHandle) -> Vec<ServiceInstanceHandle> {
        self.daemon.service_instances_for(handle)
    }

    /// Dynamically update a shelf entry for the specified service instance.
    ///
    /// This simulates external state changes (e.g., a config reload, crash recovery
    /// data arriving mid-flight). The change is immediately visible to the service
    /// on its next `unshelve()` call.
    pub fn set_shelf<T: Any + Send + Sync>(
        &self,
        handle: &ServiceInstanceHandle,
        key: &str,
        value: T,
    ) -> bool {
        self.daemon.simulation_set_shelf(handle, key, value)
    }

    /// Dynamically override the lifecycle status of a service instance.
    ///
    /// This simulates external status transitions (e.g., a dependency going unhealthy,
    /// or an operator manually marking a service for reload).
    pub fn set_status(&self, handle: &ServiceInstanceHandle, status: ServiceStatus) -> bool {
        self.daemon.simulation_set_status(handle, status)
    }

    /// Triggers a reload signal for the specified service instance.
    ///
    /// If the service has a `Watch` trigger or calls `wait_reload()`, it will
    /// be woken up immediately.
    pub fn trigger_reload(&self, handle: &ServiceInstanceHandle) -> bool {
        self.daemon.simulation_trigger_reload(handle)
    }

    /// Overrides a provider for this simulation daemon only.
    ///
    /// The override is installed into the daemon-local provider scope and is
    /// treated as a binding mutation. Existing generations that watch this
    /// provider will reload through the normal provider watch path.
    pub fn override_provider<T>(&self, value: T)
    where
        T: 'static + Send + Sync + Clone,
    {
        self.daemon.simulation_override_provider(value);
    }

    // =========================================================================
    // Safe Read API -- lock-free accessors that return owned values
    // =========================================================================

    /// Reads a shelf value by service instance handle and key, returning an owned clone.
    ///
    /// This is the **recommended** way to inspect shelf data in tests.
    /// The internal `DashMap` lock is acquired and released entirely within
    /// this call, making it safe to use across `.await` points.
    ///
    /// # Example
    /// ```rust,ignore
    /// let val: Option<String> = handle.get_shelf(&instance, "config_key");
    /// assert_eq!(val, Some("expected_value".to_string()));
    /// ```
    pub fn get_shelf<T: Any + Clone + Send + Sync>(
        &self,
        handle: &ServiceInstanceHandle,
        key: &str,
    ) -> Option<T> {
        self.daemon.simulation_get_shelf(handle, key)
    }

    /// Reads the current lifecycle status of a service, returning an owned clone.
    ///
    /// This is the **recommended** way to inspect service status in tests.
    /// The internal `DashMap` lock is acquired and released entirely within
    /// this call, making it safe to use across `.await` points.
    pub fn get_status(&self, handle: &ServiceInstanceHandle) -> Option<ServiceStatus> {
        self.daemon.simulation_get_status(handle)
    }

    /// Checks whether a shelf key exists for the specified service.
    ///
    /// Returns `true` if the key is present (regardless of its type).
    pub fn has_shelf(&self, handle: &ServiceInstanceHandle, key: &str) -> bool {
        self.daemon.simulation_has_shelf(handle, key)
    }

    /// Returns all shelf key names for the specified service.
    ///
    /// Returns an empty `Vec` if the service has no shelved data.
    pub fn shelf_keys(&self, handle: &ServiceInstanceHandle) -> Vec<String> {
        self.daemon.simulation_shelf_keys(handle)
    }
}

// =============================================================================
// MockContext -- Simulation sandbox factory
// =============================================================================

/// Simulation sandbox factory.
///
/// `MockContext` is a zero-sized type that serves as the namespace for
/// constructing simulation sandboxes via `MockContext::builder()`.
pub struct MockContext;

/// Builder for `MockContext`.
pub struct MockContextBuilder {
    resources: Arc<DaemonResources>,
    registry: Option<Registry>,
    /// Whether to auto-include framework logging services in the simulation.
    /// Default: `true` - matches production behavior.
    enable_logging: bool,
}

impl MockContext {
    /// Creates a new `MockContextBuilder` for constructing a simulation sandbox.
    pub fn builder() -> MockContextBuilder {
        MockContextBuilder {
            resources: DaemonResources::new(),
            registry: None,
            enable_logging: true,
        }
    }
}

impl MockContextBuilder {
    /// Pre-fills a shelf entry for the specified service.
    ///
    /// This simulates previously shelved data, useful for testing crash recovery
    /// and state persistence logic.
    pub fn with_shelf<T: Any + Send + Sync>(
        self,
        service_instance_id: ServiceInstanceId,
        key: &str,
        data: T,
    ) -> Self {
        {
            let entry = self.resources.shelf.entry(service_instance_id).or_default();
            entry.insert(key.to_string(), Box::new(data));
        }
        self
    }

    /// Pre-sets the lifecycle status for a specific service.
    ///
    /// This is useful for simulating the status of dependency services or
    /// setting the initial state of the service under test.
    pub fn with_status(
        self,
        service_instance_id: ServiceInstanceId,
        status: ServiceStatus,
    ) -> Self {
        self.resources
            .status_plane
            .insert(service_instance_id, status);
        self
    }

    /// Pre-installs a provider override before the simulation daemon starts.
    ///
    /// The override is scoped to this sandbox's daemon resources, so eager
    /// provider initialization and service injection see the fake value without
    /// writing into the root provider slot.
    pub fn with_provider_override<T>(self, value: T) -> Self
    where
        T: 'static + Send + Sync + Clone,
    {
        self.resources
            .provider_scope
            .override_local_slot(Arc::new(value));
        self
    }

    /// Controls whether framework logging services (`log_service`) are
    /// automatically included in the simulation registry.
    ///
    /// Default: `true` - logging services are included to match production
    /// behavior and provide consistent log output in tests.
    ///
    /// Set to `false` for lightweight tests that don't need log output.
    #[must_use]
    pub fn with_logging(mut self, enable: bool) -> Self {
        self.enable_logging = enable;
        self
    }

    /// Use a pre-built `Registry` for service discovery inside this simulation.
    #[must_use]
    pub fn with_registry(mut self, registry: Registry) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Builds the `MockContext` and returns a simulation handle.
    ///
    /// The returned handle controls a daemon instance that:
    /// - Has `Registry` isolation enabled (empty registry, no auto-discovery).
    /// - Uses a testing-friendly restart policy.
    /// - Has the pre-filled `DaemonResources` injected.
    /// - Includes framework logging services by default (controlled by `with_logging`).
    ///
    /// Call [`with_registry`](Self::with_registry) to select the real
    /// service(s) you want to debug via tag filtering.
    pub fn build(self) -> SimulationHandle {
        let mut builder = ServiceDaemonBuilder::new_isolated()
            .with_resources(self.resources)
            .with_restart_policy(RestartPolicy::for_testing());

        if let Some(registry) = self.registry {
            builder = builder.with_registry(registry);
        }

        if self.enable_logging {
            builder = builder.with_infra_tags(&["__log__"]);
        }

        SimulationHandle::new(builder.build())
    }
}
