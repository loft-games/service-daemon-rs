//! Test-only context helpers for running a real `ServiceDaemon` with injected
//! shelf, status, and provider resources.
//!
//! This module is gated behind the `simulation` feature flag and is removed
//! from production builds.

use crate::core::context::identity::DaemonResources;
use crate::core::service_daemon::{RestartPolicy, ServiceDaemonBuilder};
use crate::models::{ServiceId, ServiceStatus};

use std::any::Any;
use std::sync::Arc;

/// A handle for updating daemon resources during a simulation run.
///
/// `SimulationHandle` holds `Arc`-backed daemon resources, so updates are
/// visible to services that use the same simulation daemon.
///
/// # Example
/// ```rust,ignore
/// let (daemon, handle) = ctx.run().await;
///
/// // Phase 2: mid-flight mutation
/// handle.set_shelf::<String>(svc_id, "db_url", "new://host".into());
/// handle.set_status(svc_id, ServiceStatus::NeedReload);
/// ```
#[derive(Clone)]
pub struct SimulationHandle {
    /// Reference to the daemon's shared resources.
    resources: Arc<DaemonResources>,
}

impl SimulationHandle {
    /// Creates a new `SimulationHandle` wrapping the given resources.
    pub(crate) fn new(resources: Arc<DaemonResources>) -> Self {
        Self { resources }
    }

    /// Dynamically update a shelf entry for the specified service.
    ///
    /// This simulates external state changes (e.g., a config reload, crash recovery
    /// data arriving mid-flight). The change is immediately visible to the service
    /// on its next `unshelve()` call.
    pub fn set_shelf<T: Any + Send + Sync>(&self, service_id: ServiceId, key: &str, value: T) {
        let entry = self.resources.shelf.entry(service_id).or_default();
        entry.insert(key.to_string(), Box::new(value));
    }

    /// Dynamically override the lifecycle status of a service.
    ///
    /// This simulates external status transitions (e.g., a dependency going unhealthy,
    /// or an operator manually marking a service for reload).
    pub fn set_status(&self, service_id: ServiceId, status: ServiceStatus) {
        self.resources.status_plane.insert(service_id, status);
        // Notify any watchers that a status change occurred.
        self.resources.status_changed.notify_waiters();
    }

    /// Triggers a reload signal for the specified service.
    ///
    /// If the service has a `Watch` trigger or calls `wait_reload()`, it will
    /// be woken up immediately.
    pub fn trigger_reload(&self, service_id: &ServiceId) {
        if let Some(notify) = self.resources.reload_signals.get(service_id) {
            notify.notify_one();
        }
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
        self.resources
            .provider_scope
            .override_local_slot(Arc::new(value));
    }

    /// Returns a list of all `ServiceId`s currently visible in the status plane.
    ///
    /// This is useful for discovering the runtime IDs assigned by `Registry`,
    /// which are needed for `set_status()` and `trigger_reload()`.
    ///
    /// **Note**: Services only appear here after the runner has spawned them
    /// and written their initial status. Call this after a short delay to ensure
    /// services have been registered.
    pub fn service_ids(&self) -> Vec<ServiceId> {
        self.resources
            .status_plane
            .iter()
            .map(|entry| *entry.key())
            .collect()
    }

    // =========================================================================
    // Safe Read API -- lock-free accessors that return owned values
    // =========================================================================

    /// Reads a shelf value by service ID and key, returning an owned clone.
    ///
    /// This is the **recommended** way to inspect shelf data in tests.
    /// The internal `DashMap` lock is acquired and released entirely within
    /// this call, making it safe to use across `.await` points.
    ///
    /// # Example
    /// ```rust,ignore
    /// let val: Option<String> = handle.get_shelf(svc_id, "config_key");
    /// assert_eq!(val, Some("expected_value".to_string()));
    /// ```
    pub fn get_shelf<T: Any + Clone + Send + Sync>(
        &self,
        service_id: ServiceId,
        key: &str,
    ) -> Option<T> {
        self.resources.shelf.get(&service_id).and_then(|entry| {
            entry
                .get(key)
                .and_then(|val| val.downcast_ref::<T>().cloned())
        })
    }

    /// Reads the current lifecycle status of a service, returning an owned clone.
    ///
    /// This is the **recommended** way to inspect service status in tests.
    /// The internal `DashMap` lock is acquired and released entirely within
    /// this call, making it safe to use across `.await` points.
    pub fn get_status(&self, service_id: ServiceId) -> Option<ServiceStatus> {
        self.resources
            .status_plane
            .get(&service_id)
            .map(|s| s.value().clone())
    }

    /// Checks whether a shelf key exists for the specified service.
    ///
    /// Returns `true` if the key is present (regardless of its type).
    pub fn has_shelf(&self, service_id: ServiceId, key: &str) -> bool {
        self.resources
            .shelf
            .get(&service_id)
            .is_some_and(|entry| entry.contains_key(key))
    }

    /// Returns all shelf key names for the specified service.
    ///
    /// Returns an empty `Vec` if the service has no shelved data.
    pub fn shelf_keys(&self, service_id: ServiceId) -> Vec<String> {
        self.resources
            .shelf
            .get(&service_id)
            .map(|entry| entry.iter().map(|kv| kv.key().clone()).collect())
            .unwrap_or_default()
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
    /// Whether to auto-include framework logging services in the simulation.
    /// Default: `true` - matches production behavior.
    enable_logging: bool,
}

impl MockContext {
    /// Creates a new `MockContextBuilder` for constructing a simulation sandbox.
    pub fn builder() -> MockContextBuilder {
        MockContextBuilder {
            resources: DaemonResources::new(),
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
        service_id: ServiceId,
        key: &str,
        data: T,
    ) -> Self {
        {
            let entry = self.resources.shelf.entry(service_id).or_default();
            entry.insert(key.to_string(), Box::new(data));
        }
        self
    }

    /// Pre-sets the lifecycle status for a specific service.
    ///
    /// This is useful for simulating the status of dependency services or
    /// setting the initial state of the service under test.
    pub fn with_status(self, service_id: ServiceId, status: ServiceStatus) -> Self {
        self.resources.status_plane.insert(service_id, status);
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

    /// Builds the `MockContext` and returns a pre-configured `ServiceDaemonBuilder`
    /// along with a `SimulationHandle` for runtime updates.
    ///
    /// The returned builder:
    /// - Has `Registry` isolation enabled (empty registry, no auto-discovery).
    /// - Uses a testing-friendly restart policy.
    /// - Has the pre-filled `DaemonResources` injected.
    /// - Includes framework logging services by default (controlled by `with_logging`).
    ///
    /// You can further customize it by calling `.with_registry()` to select
    /// the real service(s) you want to debug via tag filtering.
    pub fn build(self) -> (ServiceDaemonBuilder, SimulationHandle) {
        let handle = SimulationHandle::new(self.resources.clone());

        let mut builder = ServiceDaemonBuilder::new_isolated()
            .with_resources(self.resources)
            .with_restart_policy(RestartPolicy::for_testing());

        if self.enable_logging {
            builder = builder.with_infra_tags(&["__log__"]);
        }

        (builder, handle)
    }
}
