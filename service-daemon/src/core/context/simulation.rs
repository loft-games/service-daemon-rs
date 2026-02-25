//! Testing context for service-level isolation.
//!
//! This entire module is gated behind the `simulation` feature flag and is
//! physically removed from production builds.
//!
//! ## Architecture: Interactive Simulation Sandbox
//!
//! `MockContext` acts as a **simulation sandbox factory**: it collects pre-filled
//! resources (shelf data, status overrides) and produces a `ServiceDaemonBuilder`
//! that spawns a fully real `ServiceDaemon` with those resources injected.
//!
//! After the daemon starts, a `SimulationHandle` provides "God Hand" capabilities
//! for dynamic intervention — modifying shelf data, flipping service status, or
//! triggering reload signals while the daemon is running.
//!
//! All types in this module are **strictly gated** behind `#[cfg(feature = "simulation")]`.

use crate::core::context::identity::DaemonResources;
use crate::core::service_daemon::{RestartPolicy, ServiceDaemonBuilder};
use crate::models::{ServiceId, ServiceStatus};

use std::any::Any;

// =============================================================================
// SimulationHandle — The "God Hand" for dynamic intervention
// =============================================================================

/// A handle for dynamically intervening in a running simulation.
///
/// `SimulationHandle` holds a reference to the daemon's internal `DaemonResources`
/// (which are `Arc`-based), so mutations are immediately visible to all services.
///
/// # Example
/// ```rust,ignore
/// let (daemon, handle) = ctx.run().await;
///
/// // Phase 2: mid-flight mutation
/// handle.set_shelf::<String>("config_svc", "db_url", "new://host".into());
/// handle.set_status(svc_id, ServiceStatus::NeedReload);
/// ```
#[derive(Clone)]
pub struct SimulationHandle {
    /// Reference to the daemon's shared resources.
    resources: DaemonResources,
}

impl SimulationHandle {
    /// Creates a new `SimulationHandle` wrapping the given resources.
    pub(crate) fn new(resources: DaemonResources) -> Self {
        Self { resources }
    }

    /// Dynamically update a shelf entry for the specified service.
    ///
    /// This simulates external state changes (e.g., a config reload, crash recovery
    /// data arriving mid-flight). The change is immediately visible to the service
    /// on its next `unshelve()` call.
    pub fn set_shelf<T: Any + Send + Sync>(&self, service_name: &str, key: &str, value: T) {
        let entry = self
            .resources
            .shelf
            .entry(service_name.to_string())
            .or_default();
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

    /// Returns a clone of the underlying `DaemonResources` for advanced inspection.
    ///
    /// This is useful for asserting the final state of resources after a simulation run.
    pub fn resources(&self) -> DaemonResources {
        self.resources.clone()
    }
}

// =============================================================================
// MockContext — Simulation sandbox factory
// =============================================================================

/// Simulation sandbox factory.
///
/// `MockContext` is a zero-sized type that serves as the namespace for
/// constructing simulation sandboxes via `MockContext::builder()`.
pub struct MockContext;

/// Builder for `MockContext`.
pub struct MockContextBuilder {
    resources: DaemonResources,
}

impl MockContext {
    /// Creates a new `MockContextBuilder` for constructing a simulation sandbox.
    pub fn builder() -> MockContextBuilder {
        MockContextBuilder {
            resources: DaemonResources::new(),
        }
    }
}

impl MockContextBuilder {
    /// Pre-fills a shelf entry for the specified service.
    ///
    /// This simulates previously shelved data, useful for testing crash recovery
    /// and state persistence logic.
    pub fn with_shelf<T: Any + Send + Sync>(self, service_name: &str, key: &str, data: T) -> Self {
        {
            let entry = self
                .resources
                .shelf
                .entry(service_name.to_string())
                .or_default();
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

    /// Builds the `MockContext` and returns a pre-configured `ServiceDaemonBuilder`
    /// along with a `SimulationHandle` for dynamic intervention.
    ///
    /// The returned builder:
    /// - Has `Registry` isolation enabled (empty registry, no auto-discovery).
    /// - Uses a testing-friendly restart policy.
    /// - Has the pre-filled `DaemonResources` injected.
    ///
    /// You can further customize it by calling `.with_service()` to add the
    /// real service(s) you want to debug.
    pub fn build(self) -> (ServiceDaemonBuilder, SimulationHandle) {
        let handle = SimulationHandle::new(self.resources.clone());

        let builder = ServiceDaemonBuilder::new_isolated()
            .with_resources(self.resources)
            .with_restart_policy(RestartPolicy::for_testing());

        (builder, handle)
    }
}
