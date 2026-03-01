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
//! for dynamic intervention -- modifying shelf data, flipping service status, or
//! triggering reload signals while the daemon is running.
//!
//! All types in this module are **strictly gated** behind `#[cfg(feature = "simulation")]`.

use crate::core::context::identity::DaemonResources;
use crate::core::service_daemon::{RestartPolicy, ServiceDaemonBuilder};
use crate::models::{ServiceId, ServiceStatus};

use std::any::Any;
use std::sync::Arc;

// =============================================================================
// SimulationHandle -- The "God Hand" for dynamic intervention
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

    // =========================================================================
    // Safe Read API -- lock-free accessors that return owned values
    // =========================================================================

    /// Reads a shelf value by service name and key, returning an owned clone.
    ///
    /// This is the **recommended** way to inspect shelf data in tests.
    /// The internal `DashMap` lock is acquired and released entirely within
    /// this call, making it safe to use across `.await` points.
    ///
    /// # Example
    /// ```rust,ignore
    /// let val: Option<String> = handle.get_shelf("my_service", "config_key");
    /// assert_eq!(val, Some("expected_value".to_string()));
    /// ```
    pub fn get_shelf<T: Any + Clone + Send + Sync>(
        &self,
        service_name: &str,
        key: &str,
    ) -> Option<T> {
        self.resources.shelf.get(service_name).and_then(|entry| {
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
    pub fn has_shelf(&self, service_name: &str, key: &str) -> bool {
        self.resources
            .shelf
            .get(service_name)
            .is_some_and(|entry| entry.contains_key(key))
    }

    /// Returns all shelf key names for the specified service.
    ///
    /// Returns an empty `Vec` if the service has no shelved data.
    pub fn shelf_keys(&self, service_name: &str) -> Vec<String> {
        self.resources
            .shelf
            .get(service_name)
            .map(|entry| entry.iter().map(|kv| kv.key().clone()).collect())
            .unwrap_or_default()
    }

    /// Returns a clone of the underlying `DaemonResources` for advanced inspection.
    ///
    /// # WARNING: Deadlock Risk -- Real Incident Case Study
    ///
    /// **Why this warning is here instead of in a FAQ:**
    /// If you are reaching for `resources()` to bypass [`get_shelf`] / [`get_status`],
    /// you are an advanced user who reads source code. This documentation is
    /// placed at the point of danger so you encounter it exactly when you need it.
    /// A FAQ entry would be invisible to someone skimming the API surface.
    ///
    /// ## The Problem
    ///
    /// `DashMap::get()` returns a `Ref<K, V>` that **holds an internal shard lock**
    /// for the entire lifetime of the `Ref`. These guards look like ordinary
    /// variables, but they are **invisible lock bombs**.
    ///
    /// ## Real Failure Scenario
    ///
    /// The following test code caused an **indefinite hang** in CI:
    ///
    /// ```rust,ignore
    /// // DEADLOCK -- DO NOT DO THIS
    /// let resources = handle.resources();
    /// let shelf = resources.shelf.get("svc_name").unwrap();  // holds read lock!
    /// let val = shelf.get("key").unwrap();                    // holds another read lock!
    /// assert_eq!(val.value().downcast_ref::<String>(), ...);
    /// // locks are still alive here...
    ///
    /// cancel.cancel();
    /// daemon_task.await;  // <-- DEADLOCK: daemon waits for service to stop,
    ///                     //   service calls shelve() which needs write lock,
    ///                     //   but test still holds read lock above.
    /// ```
    ///
    /// ## Circular Wait Diagram
    ///
    /// ```text
    /// Test thread               Daemon / Service thread
    /// -------------             ----------------------
    /// shelf.get("svc")          (running service loop)
    ///   | holds read lock
    /// shelf.get("key")
    ///   | holds read lock
    /// cancel.cancel()
    ///   |
    /// daemon_task.await ------> stop_all_services()
    ///   (blocked)                 | cancels service token
    ///                          service loop exits
    ///                            | calls shelve()
    ///                          shelf.entry("svc").insert()
    ///                            | needs WRITE lock
    ///                          BLOCKED by test's read lock
    ///                            ^
    ///                          == circular wait ==
    /// ```
    ///
    /// ## Safe Alternative
    ///
    /// Use the lock-free accessors instead -- they acquire and release the lock
    /// within a single synchronous call, making cross-await deadlocks impossible:
    ///
    /// ```rust,ignore
    /// // SAFE -- lock released before any .await
    /// let val: Option<String> = handle.get_shelf("svc_name", "key");
    /// assert_eq!(val, Some("expected".to_string()));
    ///
    /// cancel.cancel();
    /// daemon_task.await;  // no lock held, no deadlock
    /// ```
    ///
    /// ## If You Must Use `resources()`
    ///
    /// Scope every `DashMap::Ref` inside a `{ ... }` block so the lock is
    /// dropped before any `.await`:
    ///
    /// ```rust,ignore
    /// let val = {
    ///     let resources = handle.resources();
    ///     let shelf = resources.shelf.get("svc").unwrap();
    ///     shelf.get("key").unwrap().value().downcast_ref::<String>().cloned()
    /// }; // <-- all locks dropped here
    ///
    /// cancel.cancel();
    /// daemon_task.await;  // safe
    /// ```
    #[doc(hidden)]
    pub fn resources(&self) -> Arc<DaemonResources> {
        self.resources.clone()
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
