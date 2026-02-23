use dashmap::DashMap;
use std::any::Any;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::task_local;
use tokio_util::sync::CancellationToken;

use crate::models::ServiceStatus;

// Type aliases for the Shelf
type ShelfValue = Box<dyn Any + Send + Sync>;
type ServiceShelf = DashMap<String, ShelfValue>;
type GlobalShelfMapping = DashMap<String, ServiceShelf>;

/// Shared daemon resources that are owned by `ServiceDaemon` and plumbed to services.
///
/// This struct holds the references to daemon-managed resources. It is passed
/// to each service via the `CURRENT_SERVICE` task-local, enabling services to
/// interact with the daemon's state plane, shelf, and signaling mechanisms
/// without polluting the global namespace.
#[derive(Clone)]
pub struct DaemonResources {
    /// The unified Status Plane: stores the current lifecycle status for each service.
    pub status_plane: Arc<DashMap<String, ServiceStatus>>,
    /// Shelf for cross-generational state persistence (managed values).
    /// Structure: DashMap<ServiceName, DashMap<Key, Value>>
    pub shelf: Arc<GlobalShelfMapping>,
    /// Signals for services to reload.
    pub reload_signals: Arc<DashMap<String, Arc<tokio::sync::Notify>>>,
    /// Global notification for any status change in the STATUS_PLANE.
    pub status_changed: Arc<tokio::sync::Notify>,
}

impl DaemonResources {
    /// Creates a new set of daemon resources.
    pub fn new() -> Self {
        Self {
            status_plane: Arc::new(DashMap::new()),
            shelf: Arc::new(DashMap::new()),
            reload_signals: Arc::new(DashMap::new()),
            status_changed: Arc::new(tokio::sync::Notify::new()),
        }
    }
}

impl Default for DaemonResources {
    fn default() -> Self {
        Self::new()
    }
}

/// Internal identity of a service used to link task-local calls to the daemon's management.
///
/// This is a lightweight handle containing only the lifecycle tokens and a local
/// "handshake done" flag. The actual daemon resources are stored separately in
/// `CURRENT_RESOURCES` (internal, not exposed to users).
#[derive(Clone)]
pub struct ServiceIdentity {
    pub name: String,
    pub cancellation_token: CancellationToken,
    pub reload_token: CancellationToken,
    /// Shared flag: true means the auto-handshake (Initializing->Healthy) has been performed.
    /// Uses Arc to persist the state across TLS clones within the same task generation.
    is_handshake_done: Arc<AtomicBool>,
}

impl ServiceIdentity {
    /// Creates a new ServiceIdentity with the handshake flag set to false.
    pub fn new(
        name: String,
        cancellation_token: CancellationToken,
        reload_token: CancellationToken,
    ) -> Self {
        Self {
            name,
            cancellation_token,
            reload_token,
            is_handshake_done: Arc::new(AtomicBool::new(false)),
        }
    }
}

task_local! {
    /// Internal: The identity of the currently running service task.
    pub(crate) static CURRENT_SERVICE: ServiceIdentity;
    /// Internal: The daemon resources for the current service task.
    pub(crate) static CURRENT_RESOURCES: DaemonResources;
}

// ──────────────────────────────────────────────────────────────────────────────
// Simulation Overlay (feature-gated: "simulation")
// ──────────────────────────────────────────────────────────────────────────────
#[cfg(feature = "simulation")]
mod simulation {
    use super::*;
    use std::any::TypeId;

    /// A type-erased store for Provider shadow snapshots.
    ///
    /// Each entry maps a `TypeId` to an `Arc<T>` stored as `Arc<dyn Any + Send + Sync>`.
    /// This allows `MockContext` to inject arbitrary provider values without touching
    /// the global `StateManager` singletons.
    #[derive(Clone, Default)]
    pub struct SimulationOverlay {
        pub(crate) providers: Arc<DashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
    }

    impl SimulationOverlay {
        /// Attempts to retrieve a shadow snapshot for type `T`.
        /// Returns `Some(Arc<T>)` if a mock was registered, `None` otherwise.
        pub fn get<T: 'static + Send + Sync>(&self) -> Option<Arc<T>> {
            self.providers
                .get(&TypeId::of::<T>())
                .and_then(|entry| entry.value().clone().downcast::<T>().ok())
        }

        /// Inserts a shadow snapshot for type `T`.
        pub fn insert<T: 'static + Send + Sync>(&self, value: T) {
            self.providers.insert(TypeId::of::<T>(), Arc::new(value));
        }
    }

    task_local! {
        /// Task-local simulation overlay for Provider shadow snapshots.
        pub(crate) static SIMULATION_OVERLAY: SimulationOverlay;
    }

    /// Builder for constructing a `MockContext` with injected mock data.
    ///
    /// `MockContext` acts as an Environment Proxy, providing isolated shadow data
    /// for Providers, Shelf, and service Status within a scoped `task_local`
    /// execution context. All simulation logic is gated behind the `simulation`
    /// feature flag and is physically removed from production builds.
    ///
    /// # Example
    /// ```rust,ignore
    /// use service_daemon::utils::context::MockContext;
    ///
    /// #[tokio::test]
    /// async fn test_with_mocks() {
    ///     let ctx = MockContext::builder()
    ///         .with_mock::<AppConfig>(AppConfig { api_key: "test".into() })
    ///         .with_shelf::<MyService>("checkpoint", SavedState { counter: 42 })
    ///         .with_status(ServiceStatus::Healthy)
    ///         .with_log_drain()
    ///         .build();
    ///
    ///     ctx.run(|| async {
    ///         // Business logic transparently hits shadow data
    ///         let config = AppConfig::resolve().await;
    ///         assert_eq!(config.api_key, "test");
    ///     }).await;
    /// }
    /// ```
    pub struct MockContext {
        /// Isolated DaemonResources (status plane + shelf).
        pub(crate) resources: DaemonResources,
        /// Shadow Provider snapshots.
        pub(crate) overlay: SimulationOverlay,
        /// The service identity to use within the mock scope.
        pub(crate) identity: ServiceIdentity,
        /// Whether to drain the internal log queue during execution.
        pub(crate) has_log_drain: bool,
    }

    /// Builder for `MockContext`.
    pub struct MockContextBuilder {
        resources: DaemonResources,
        overlay: SimulationOverlay,
        service_name: String,
        has_log_drain: bool,
    }

    impl MockContext {
        /// Creates a new `MockContextBuilder` for constructing a simulation environment.
        pub fn builder() -> MockContextBuilder {
            MockContextBuilder {
                resources: DaemonResources::new(),
                overlay: SimulationOverlay::default(),
                service_name: "mock_service".to_string(),
                has_log_drain: false,
            }
        }

        /// Executes the given async closure within the mock environment.
        ///
        /// All calls to `resolve()`, `state()`, `shelve()`, and `unshelve()` inside
        /// the closure will transparently hit the shadow data injected via the builder,
        /// without touching any global static state.
        #[allow(clippy::let_and_return)]
        pub async fn run<F, Fut, R>(&self, f: F) -> R
        where
            F: FnOnce() -> Fut,
            Fut: Future<Output = R>,
        {
            // Optionally spawn a background log drain task.
            // The `_drain_handle` MUST be kept alive (not dropped) until after `result`
            // is obtained, so the intentional `let result = ...` pattern is correct.
            let _drain_handle = if self.has_log_drain {
                Some(spawn_log_drain())
            } else {
                None
            };

            let result = SIMULATION_OVERLAY
                .scope(
                    self.overlay.clone(),
                    CURRENT_SERVICE.scope(
                        self.identity.clone(),
                        CURRENT_RESOURCES.scope(self.resources.clone(), f()),
                    ),
                )
                .await;

            // `_drain_handle` drops here, stopping the background consumer.
            result
        }
    }

    impl MockContextBuilder {
        /// Sets the service name used for shelf isolation and identity.
        pub fn with_service_name(mut self, name: impl Into<String>) -> Self {
            self.service_name = name.into();
            self
        }

        /// Injects a shadow Provider snapshot for type `T`.
        ///
        /// When business code calls `T::resolve()` inside `MockContext::run()`,
        /// it will receive this value instead of initializing from the real provider.
        pub fn with_mock<T: 'static + Send + Sync + Clone>(self, value: T) -> Self {
            self.overlay.insert(value);
            self
        }

        /// Pre-fills a Shelf entry for the configured service.
        ///
        /// This simulates previously shelved data, useful for testing crash recovery
        /// and state persistence logic.
        pub fn with_shelf<T: Any + Send + Sync>(self, key: &str, data: T) -> Self {
            // Scope the DashMap RefMut guard so it drops before `self` is moved.
            {
                let entry = self
                    .resources
                    .shelf
                    .entry(self.service_name.clone())
                    .or_default();
                entry.insert(key.to_string(), Box::new(data));
            }
            self
        }

        /// Sets the initial lifecycle status for the mock service.
        pub fn with_status(self, status: ServiceStatus) -> Self {
            self.resources
                .status_plane
                .insert(self.service_name.clone(), status);
            self
        }

        /// Sets the lifecycle status for a specific service in the mock environment.
        ///
        /// This is useful for mocking the status of dependency services that the
        /// current service might be watching or interacting with.
        pub fn with_service_status(self, name: &str, status: ServiceStatus) -> Self {
            self.resources.status_plane.insert(name.to_string(), status);
            self
        }

        /// Enables automatic log queue draining during `MockContext::run()`.
        ///
        /// When enabled, a background task subscribes to the internal `LogQueue`
        /// and prints all captured log events to stderr. This prevents the
        /// "log black hole" problem where logs are silently discarded because
        /// the `LogService` is not running in unit test environments.
        pub fn with_log_drain(mut self) -> Self {
            self.has_log_drain = true;
            self
        }

        /// Builds the `MockContext` from the configured builder state.
        pub fn build(self) -> MockContext {
            MockContext {
                resources: self.resources,
                overlay: self.overlay,
                identity: ServiceIdentity::new(
                    self.service_name,
                    CancellationToken::new(),
                    CancellationToken::new(),
                ),
                has_log_drain: self.has_log_drain,
            }
        }
    }

    /// Spawns a background task that drains the internal log queue to stderr.
    ///
    /// Returns a `JoinHandle` that, when dropped (via `abort_on_drop`), will stop
    /// the drain task. This ensures the drain lives only as long as the `MockContext::run` scope.
    fn spawn_log_drain() -> tokio::task::JoinHandle<()> {
        tokio::spawn(async {
            let mut rx = crate::utils::logging::subscribe_log_queue();
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        eprintln!(
                            "[MockContext] [{}] {:<5} [{}] {}",
                            event.timestamp.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
                            event.level,
                            event.target,
                            event.message
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        eprintln!("[MockContext] LogDrain lagged by {} messages", n);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    }

    /// Checks the simulation overlay for a shadow snapshot of type `T`.
    pub(crate) fn try_resolve_mock_internal<T: 'static + Send + Sync>() -> Option<Arc<T>> {
        SIMULATION_OVERLAY
            .try_with(|overlay| overlay.get::<T>())
            .ok()
            .flatten()
    }
}

#[cfg(feature = "simulation")]
pub use simulation::{MockContext, MockContextBuilder, SimulationOverlay};

/// Checks the simulation overlay for a shadow snapshot of type `T`.
///
/// This is called by macro-generated `Provided::resolve()` implementations
/// to intercept resolution when running under a `MockContext`.
///
/// **Zero Overhead**: In production builds (simulation feature disabled),
/// this function is a no-op that always returns `None`. Due to `#[inline(always)]`,
/// the compiler will completely optimize away the overhead.
#[inline(always)]
pub fn try_resolve_mock<T: 'static + Send + Sync>() -> Option<Arc<T>> {
    #[cfg(feature = "simulation")]
    {
        simulation::try_resolve_mock_internal::<T>()
    }
    #[cfg(not(feature = "simulation"))]
    {
        None
    }
}

/// Runs a future within the context of a service.
///
/// This is the internal entry point used by the `#[service]` and `#[trigger]` macros.
/// It sets up the task-local identity and resources before executing the user's code.
#[doc(hidden)]
pub async fn __run_service_scope<F, Fut>(
    identity: ServiceIdentity,
    resources: DaemonResources,
    f: F,
) -> Fut::Output
where
    F: FnOnce() -> Fut,
    Fut: Future,
{
    CURRENT_SERVICE
        .scope(identity, CURRENT_RESOURCES.scope(resources, f()))
        .await
}

/// Returns the current lifecycle status of the calling service.
pub fn state() -> ServiceStatus {
    let id = match CURRENT_SERVICE.try_with(|id| id.clone()) {
        Ok(id) => id,
        Err(_) => return ServiceStatus::Initializing,
    };

    // Fast path: Check cancellation tokens first (atomic, no locking)
    if id.cancellation_token.is_cancelled() {
        return ServiceStatus::ShuttingDown;
    }
    if id.reload_token.is_cancelled() {
        // Need to check if daemon already marked ShuttingDown
        if let Ok(resources) = CURRENT_RESOURCES.try_with(|r| r.clone())
            && let Some(status) = resources.status_plane.get(&id.name)
            && matches!(status.value(), ServiceStatus::ShuttingDown)
        {
            return ServiceStatus::ShuttingDown;
        }
        return ServiceStatus::NeedReload;
    }

    // Full status lookup
    CURRENT_RESOURCES
        .try_with(|r| {
            r.status_plane
                .get(&id.name)
                .map(|s| s.value().clone())
                .unwrap_or(ServiceStatus::Initializing)
        })
        .unwrap_or(ServiceStatus::Initializing)
}

/// Signals that the service has completed its current state (e.g. initialization or cleanup).
/// This will advance the service status to the next logical step based on the handshake protocol:
/// - `Initializing | Restoring | Recovering` -> `Healthy` (service is now ready).
/// - `NeedReload | ShuttingDown` -> `Terminated` (service is ready for collection).
/// - Otherwise, no-op.
pub fn done() {
    let id = match CURRENT_SERVICE.try_with(|id| id.clone()) {
        Ok(id) => id,
        Err(_) => return,
    };
    let resources = match CURRENT_RESOURCES.try_with(|r| r.clone()) {
        Ok(r) => r,
        Err(_) => return,
    };

    let current_status = resources
        .status_plane
        .get(&id.name)
        .map(|s| s.value().clone())
        .unwrap_or(ServiceStatus::Initializing);

    let next_status = match &current_status {
        ServiceStatus::Initializing | ServiceStatus::Restoring | ServiceStatus::Recovering(_) => {
            ServiceStatus::Healthy
        }
        ServiceStatus::NeedReload | ServiceStatus::ShuttingDown => ServiceStatus::Terminated,
        _ => current_status.clone(), // No-op for Healthy and Terminated
    };

    if next_status != current_status {
        resources
            .status_plane
            .insert(id.name.clone(), next_status.clone());
        resources.status_changed.notify_waiters();
        tracing::info!(
            "Service '{}' signalled done() (Transition: {:?} -> {:?})",
            id.name,
            current_status,
            next_status
        );
    }
}

/// Shelves a managed value to the daemon. This value will survive service reloads and crashes.
/// The value is stored in a service-isolated bucket based on the calling service's identity.
pub async fn shelve<T: Any + Send + Sync>(key: &str, data: T) {
    let name = match CURRENT_SERVICE.try_with(|id| id.name.clone()) {
        Ok(n) => n,
        Err(_) => return,
    };
    if let Ok(resources) = CURRENT_RESOURCES.try_with(|r| r.clone()) {
        let entry = resources.shelf.entry(name).or_default();
        entry.insert(key.to_string(), Box::new(data));
    }
}

/// Retrieves a shelved managed value previously submitted by this service.
/// The value is retrieved from the service's isolated bucket.
pub async fn unshelve<T: Any + Send + Sync>(key: &str) -> Option<T> {
    let name = match CURRENT_SERVICE.try_with(|id| id.name.clone()) {
        Ok(n) => n,
        Err(_) => return None,
    };
    CURRENT_RESOURCES
        .try_with(|r| {
            r.shelf.get(&name).and_then(|entry| {
                entry
                    .remove(key)
                    .and_then(|(_, val)| val.downcast::<T>().ok().map(|b| *b))
            })
        })
        .ok()
        .flatten()
}

/// Performs an implicit handshake if the service is still in a "Starting" phase.
/// This is an optimized version that uses a local flag to avoid repeated global lookups.
fn implicit_handshake() {
    let id = match CURRENT_SERVICE.try_with(|id| id.clone()) {
        Ok(id) => id,
        Err(_) => return,
    };

    // Fast path: If already handshaked this generation, skip entirely
    if id.is_handshake_done.load(Ordering::Relaxed) {
        return;
    }

    let resources = match CURRENT_RESOURCES.try_with(|r| r.clone()) {
        Ok(r) => r,
        Err(_) => return,
    };

    // Check and transition startup states
    let needs_transition = resources
        .status_plane
        .get(&id.name)
        .map(|s| {
            matches!(
                s.value(),
                ServiceStatus::Initializing
                    | ServiceStatus::Restoring
                    | ServiceStatus::Recovering(_)
            )
        })
        .unwrap_or(false);

    if needs_transition {
        resources
            .status_plane
            .insert(id.name.clone(), ServiceStatus::Healthy);
        resources.status_changed.notify_waiters();
        tracing::debug!(
            "Service '{}' implicitly transitioned to Healthy (via lifecycle utility)",
            id.name
        );
    }

    // Mark as done for this task; subsequent calls skip all of the above
    id.is_handshake_done.store(true, Ordering::Relaxed);
}

/// Checks if the current service or the daemon has been signaled to stop or reload.
/// Returns `true` for any "descending" status (NeedReload, ShuttingDown, Terminated).
///
/// **Note**: If the service is still in a "Starting" phase, this function will
/// implicitly transition it to `Healthy` status (once per task lifetime).
pub fn is_shutdown() -> bool {
    implicit_handshake();

    // Fast path: Check tokens directly (atomic, no locking)
    if let Ok(id) = CURRENT_SERVICE.try_with(|id| id.clone())
        && (id.cancellation_token.is_cancelled() || id.reload_token.is_cancelled())
    {
        return true;
    }
    false
}

/// Waits until the service is notified to stop or reload.
/// This future completes when the service's cancellation or reload token is triggered.
///
/// **Note**: If the service is still in a "Starting" phase, this function will
/// implicitly transition it to `Healthy` status.
pub async fn wait_shutdown() {
    implicit_handshake();
    if let Ok(id) = CURRENT_SERVICE.try_with(|id| id.clone()) {
        tokio::select! {
            _ = id.cancellation_token.cancelled() => {}
            _ = id.reload_token.cancelled() => {}
        }
    }
}

/// An interruptible sleep that returns early if a shutdown or reload signal is received.
/// Returns `true` if the sleep completed normally, `false` if interrupted.
///
/// **Note**: If the service is still in a "Starting" phase, this function will
/// implicitly transition it to `Healthy` status.
pub async fn sleep(duration: Duration) -> bool {
    implicit_handshake();
    if let Ok(id) = CURRENT_SERVICE.try_with(|id| id.clone()) {
        tokio::select! {
            _ = tokio::time::sleep(duration) => true,
            _ = id.cancellation_token.cancelled() => false,
            _ = id.reload_token.cancelled() => false,
        }
    } else {
        // Outside of a service context, just perform a regular sleep
        tokio::time::sleep(duration).await;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_resources() -> DaemonResources {
        DaemonResources::new()
    }

    fn create_test_identity(name: &str) -> ServiceIdentity {
        ServiceIdentity::new(
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
            .insert("state_service".to_string(), ServiceStatus::NeedReload);

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
            .insert("handshake_service".to_string(), ServiceStatus::Initializing);

        let identity = create_test_identity("handshake_service");
        let resources_clone = resources.clone();
        in_scope(identity, resources.clone(), || async move {
            // After done(), status should become Healthy
            done();
            let status = resources_clone
                .status_plane
                .get("handshake_service")
                .map(|s| s.clone());
            assert_eq!(status, Some(ServiceStatus::Healthy));
        })
        .await;

        // Now test the descending phase
        resources
            .status_plane
            .insert("handshake_service".to_string(), ServiceStatus::NeedReload);

        let identity2 = create_test_identity("handshake_service");
        let resources_clone2 = resources.clone();
        in_scope(identity2, resources.clone(), || async move {
            // After done(), status should become Terminated
            done();
            let status = resources_clone2
                .status_plane
                .get("handshake_service")
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
            .insert("isolated_svc".to_string(), ServiceStatus::Healthy);
        resources_b
            .status_plane
            .insert("isolated_svc".to_string(), ServiceStatus::Initializing);

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
            .insert("opt_svc".to_string(), ServiceStatus::Initializing);

        let identity = create_test_identity("opt_svc");
        let resources_clone = resources.clone();

        in_scope(identity, resources, || async move {
            // First call should trigger handshake
            assert!(!is_shutdown());

            // Status should now be Healthy
            let status = resources_clone
                .status_plane
                .get("opt_svc")
                .map(|s| s.clone());
            assert_eq!(status, Some(ServiceStatus::Healthy));

            // Revert status to Initializing to prove the flag prevents re-handshake
            resources_clone
                .status_plane
                .insert("opt_svc".to_string(), ServiceStatus::Initializing);

            // Second call should NOT re-handshake (flag is set)
            assert!(!is_shutdown());

            // Status should remain Initializing because handshake was skipped
            let status2 = resources_clone
                .status_plane
                .get("opt_svc")
                .map(|s| s.clone());
            assert_eq!(status2, Some(ServiceStatus::Initializing));
        })
        .await;
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Simulation Tests (feature-gated: "simulation")
// ──────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
#[cfg(feature = "simulation")]
mod simulation_tests {
    use super::*;

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
            .with_service_status("dependency_svc", ServiceStatus::NeedReload)
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
                    .get("dependency_svc")
                    .map(|s| s.value().clone())
            });
            assert_eq!(dep_status, Some(ServiceStatus::NeedReload));
        })
        .await;
    }
}
