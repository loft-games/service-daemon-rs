//! Simulation overlay for testing with `MockContext`.
//!
//! This entire module is gated behind the `simulation` feature flag and is
//! physically removed from production builds.

use super::identity::{CURRENT_RESOURCES, CURRENT_SERVICE, DaemonResources, ServiceIdentity};
use crate::models::{ServiceId, ServiceStatus};

use dashmap::DashMap;
use std::any::{Any, TypeId};
use std::future::Future;
use std::sync::Arc;
use tokio::task_local;
use tokio_util::sync::CancellationToken;

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
/// use service_daemon::core::context::MockContext;
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
    service_id: ServiceId,
    service_name: String,
    has_log_drain: bool,
}

impl MockContext {
    /// Creates a new `MockContextBuilder` for constructing a simulation environment.
    pub fn builder() -> MockContextBuilder {
        MockContextBuilder {
            resources: DaemonResources::new(),
            overlay: SimulationOverlay::default(),
            service_id: ServiceId::new(0),
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
        self.resources.status_plane.insert(self.service_id, status);
        self
    }

    /// Sets the lifecycle status for a specific service in the mock environment.
    ///
    /// This is useful for mocking the status of dependency services that the
    /// current service might be watching or interacting with.
    pub fn with_service_status(self, id: ServiceId, status: ServiceStatus) -> Self {
        self.resources.status_plane.insert(id, status);
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
                self.service_id,
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
        let mut rx = crate::core::logging::subscribe_log_queue();
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
