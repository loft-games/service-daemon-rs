//! ServiceDaemon - the main orchestrator for managed services.
//!
//! This module is split into submodules for better organization:
//! - `policy`: Restart policy configuration.
//! - `runner`: Service spawning and lifecycle management.

mod policy;
mod runner;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument};

use crate::core::context::DaemonResources;
use crate::models::{
    Registry, Result as ServiceResult, ServiceDescription, ServiceId, ServiceStatus,
};

pub use policy::{RestartPolicy, RestartPolicyBuilder};

// ---------------------------------------------------------------------------
// ServiceDaemonHandle -- lightweight status query interface
// ---------------------------------------------------------------------------

/// A handle to the ServiceDaemon that can be used to query status and interact with services.
#[derive(Clone)]
pub struct ServiceDaemonHandle {
    resources: Arc<DaemonResources>,
}

impl ServiceDaemonHandle {
    /// Get the current status of a service by its `ServiceId`.
    pub async fn get_service_status(&self, id: &ServiceId) -> ServiceStatus {
        self.resources
            .status_plane
            .get(id)
            .map(|s| s.clone())
            .unwrap_or(ServiceStatus::Terminated)
    }
}

// ---------------------------------------------------------------------------
// ServiceDaemon -- Infallible Builder pattern
// ---------------------------------------------------------------------------

/// The main orchestrator for managed services.
///
/// `ServiceDaemon` acts as both a lifecycle manager and a control handle.
/// After calling [`run()`](ServiceDaemon::run), the daemon starts services
/// in the background and returns control to the caller. Use
/// [`wait()`](ServiceDaemon::wait) to block until shutdown, or
/// [`shutdown()`](ServiceDaemon::shutdown) to trigger graceful termination.
///
/// # Examples
/// ```rust,ignore
/// // Non-blocking start, then wait for Ctrl+C:
/// let mut daemon = ServiceDaemon::builder().build();
/// daemon.run().await;
/// daemon.wait().await?;
///
/// // Hierarchical integration with external CancellationToken:
/// let root_token = CancellationToken::new();
/// let mut daemon = ServiceDaemon::builder()
///     .with_cancel_token(root_token.clone())
///     .build();
/// daemon.run().await;
/// // ... other work using root_token ...
/// daemon.wait().await?;
/// ```
pub struct ServiceDaemon {
    services: Vec<ServiceDescription>,
    running_tasks: Arc<Mutex<HashMap<ServiceId, JoinHandle<()>>>>,
    restart_policy: RestartPolicy,
    cancellation_token: CancellationToken,
    /// Optional external token for hierarchical lifecycle management.
    /// When cancelled, the daemon treats it as a shutdown signal.
    external_cancel_token: Option<CancellationToken>,
    /// Instance-owned resources (Status Plane, Shelf, Signals)
    resources: Arc<DaemonResources>,
}

impl ServiceDaemon {
    /// Start building a new `ServiceDaemon`.
    #[must_use]
    pub fn builder() -> ServiceDaemonBuilder {
        ServiceDaemonBuilder::new()
    }

    /// Get the cancellation token for this daemon.
    pub fn cancel_token(&self) -> tokio_util::sync::CancellationToken {
        self.cancellation_token.clone()
    }

    /// Get a handle to the daemon for querying status.
    pub fn handle(&self) -> ServiceDaemonHandle {
        ServiceDaemonHandle {
            resources: self.resources.clone(),
        }
    }

    /// **[Simulation Only]** Returns a clone of the daemon's internal resources.
    ///
    /// This is used by `SimulationHandle` to perform dynamic injection ("God Hand")
    /// during a running simulation. Since `DaemonResources` uses `Arc` internally,
    /// modifications through the returned clone are immediately visible to all services.
    ///
    /// # Safety
    /// This method is gated behind the `simulation` feature to prevent misuse
    /// in production environments.
    #[cfg(feature = "simulation")]
    pub fn resources(&self) -> Arc<DaemonResources> {
        self.resources.clone()
    }

    /// Get the current status of a service by its `ServiceId`.
    pub async fn get_service_status(&self, id: &ServiceId) -> ServiceStatus {
        self.handle().get_service_status(id).await
    }

    /// Start the daemon in the background (non-blocking).
    ///
    /// This method spawns all registered services using wave-based priorities
    /// and returns immediately. The daemon continues running in the background.
    ///
    /// Use [`wait()`](ServiceDaemon::wait) to block until a shutdown signal,
    /// or [`shutdown()`](ServiceDaemon::shutdown) to trigger graceful termination.
    #[instrument(skip(self))]
    pub async fn run(&mut self) -> &mut Self {
        if self.services.is_empty() {
            info!("ServiceDaemon has no services to run. Daemon started in idle mode.");
        }

        // Spawn all services in the background
        runner::spawn_all_services(
            &self.services,
            self.restart_policy,
            self.running_tasks.clone(),
            self.resources.clone(),
            &self.cancellation_token,
        )
        .await;

        info!(
            "ServiceDaemon running with {} service(s).",
            self.services.len()
        );

        self
    }

    /// Wait for the daemon to stop.
    ///
    /// This method blocks until one of the following events occurs:
    /// - An OS signal is received (SIGINT / SIGTERM / Ctrl+C).
    /// - The internal cancellation token is cancelled (via [`shutdown()`](ServiceDaemon::shutdown)).
    /// - An external cancellation token is cancelled (if provided via
    ///   [`with_cancel_token()`](ServiceDaemonBuilder::with_cancel_token)).
    ///
    /// After the trigger event, this method performs a graceful shutdown
    /// of all services using wave-based priorities.
    ///
    /// # Signal Guard (Layer 1 Defense)
    /// If signal handler registration fails (e.g. restricted container environment),
    /// this method returns `Err` immediately to prevent an uncontrollable daemon.
    #[instrument(skip(self))]
    pub async fn wait(&mut self) -> ServiceResult<()> {
        // Wait for shutdown signal (Ctrl+C, SIGTERM, or token cancellation)
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let mut sigint = signal(SignalKind::interrupt()).map_err(|e| {
                crate::models::ServiceError::InternalError(format!("Failed to setup SIGINT: {}", e))
            })?;
            let mut sigterm = signal(SignalKind::terminate()).map_err(|e| {
                crate::models::ServiceError::InternalError(format!(
                    "Failed to setup SIGTERM: {}",
                    e
                ))
            })?;

            tokio::select! {
                _ = sigint.recv() => {
                    info!("Received SIGINT, shutting down...");
                }
                _ = sigterm.recv() => {
                    info!("Received SIGTERM, shutting down...");
                }
                _ = self.cancellation_token.cancelled() => {
                    info!("Received internal cancellation signal, shutting down...");
                }
                _ = Self::wait_external_token(&self.external_cancel_token) => {
                    info!("Received external cancellation signal, shutting down...");
                }
            }
        }

        #[cfg(not(unix))]
        {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("Received Ctrl+C, shutting down...");
                }
                _ = self.cancellation_token.cancelled() => {
                    info!("Received internal cancellation signal, shutting down...");
                }
                _ = Self::wait_external_token(&self.external_cancel_token) => {
                    info!("Received external cancellation signal, shutting down...");
                }
            }
        }

        // Graceful shutdown
        self.do_shutdown().await;

        Ok(())
    }

    /// Trigger graceful shutdown of the daemon.
    ///
    /// This cancels the internal `CancellationToken`, which will cause
    /// [`wait()`](ServiceDaemon::wait) to proceed with the shutdown sequence.
    /// If an external token was provided, it is also cancelled to propagate
    /// the shutdown signal to other components sharing that token.
    pub fn shutdown(&self) {
        info!("ServiceDaemon::shutdown() called, triggering graceful termination...");
        self.cancellation_token.cancel();
        // Propagate shutdown to external token if present
        if let Some(ref external) = self.external_cancel_token {
            external.cancel();
        }
    }

    /// Internal helper: perform the actual graceful shutdown sequence.
    async fn do_shutdown(&self) {
        runner::stop_all_services(
            &self.services,
            self.running_tasks.clone(),
            self.resources.clone(),
            self.cancellation_token.clone(),
            self.restart_policy.wave_stop_timeout,
        )
        .await;
        info!("ServiceDaemon stopped.");
    }

    /// Internal helper: wait on an external CancellationToken if present.
    /// If no external token was provided, this future never resolves.
    async fn wait_external_token(token: &Option<CancellationToken>) {
        match token {
            Some(t) => t.cancelled().await,
            None => std::future::pending().await,
        }
    }

    /// Run for a limited duration (for testing).
    #[allow(dead_code)]
    #[instrument(skip(self))]
    pub async fn run_for_duration(self, duration: Duration) -> ServiceResult<()> {
        // Use testing policy with shorter delays
        let test_policy = RestartPolicy::for_testing();

        for service in &self.services {
            runner::spawn_service(
                service.id,
                service.name.clone(),
                service.run.clone(),
                service.watcher.clone(),
                test_policy,
                self.running_tasks.clone(),
                self.resources.clone(),
                service.cancellation_token.clone(),
            )
            .await;
        }

        tokio::time::sleep(duration).await;

        runner::stop_all_services(
            &self.services,
            self.running_tasks.clone(),
            self.resources.clone(),
            self.cancellation_token.clone(),
            test_policy.wave_stop_timeout,
        )
        .await;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ServiceDaemonBuilder -- Infallible, zero-config default
// ---------------------------------------------------------------------------

/// Builder for constructing a `ServiceDaemon`.
///
/// The `.build()` method is **infallible** -- it always returns a valid daemon.
pub struct ServiceDaemonBuilder {
    registry: Option<Registry>,
    restart_policy: RestartPolicy,
    extra_services: Vec<ServiceDescription>,
    /// External cancellation token for hierarchical lifecycle management.
    external_cancel_token: Option<CancellationToken>,
    /// Type-erased trigger configuration overrides.
    trigger_configs: dashmap::DashMap<std::any::TypeId, Box<dyn std::any::Any + Send + Sync>>,
    /// Pre-filled resources for simulation (only available with `simulation` feature).
    #[cfg(feature = "simulation")]
    resources: Option<Arc<DaemonResources>>,
}

impl ServiceDaemonBuilder {
    fn new() -> Self {
        Self {
            registry: None,
            restart_policy: RestartPolicy::default(),
            extra_services: Vec::new(),
            external_cancel_token: None,
            trigger_configs: dashmap::DashMap::new(),
            #[cfg(feature = "simulation")]
            resources: None,
        }
    }

    /// **[Simulation Only]** Creates an isolated builder with an empty registry.
    ///
    /// This prevents auto-discovery of statically registered services, ensuring
    /// the simulation sandbox only runs explicitly added services.
    #[cfg(feature = "simulation")]
    pub(crate) fn new_isolated() -> Self {
        Self {
            registry: Some(
                crate::models::Registry::builder()
                    .with_tag("__simulation_isolation__")
                    .build(),
            ),
            restart_policy: RestartPolicy::default(),
            extra_services: Vec::new(),
            external_cancel_token: None,
            trigger_configs: dashmap::DashMap::new(),
            resources: None,
        }
    }

    /// Use a pre-built `Registry` for service discovery.
    ///
    /// If not called, the daemon will automatically include all services
    /// discovered via the static `SERVICE_REGISTRY` (linkme).
    #[must_use]
    pub fn with_registry(mut self, registry: Registry) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Set a custom restart policy for the daemon.
    #[must_use]
    pub fn with_restart_policy(mut self, policy: RestartPolicy) -> Self {
        self.restart_policy = policy;
        self
    }

    /// Add a manually constructed `ServiceDescription` to the daemon.
    ///
    /// This is the primary way to inject ad-hoc services in integration tests
    /// without going through the static `#[service]` registration pipeline.
    ///
    /// **Note**: You are responsible for assigning unique `ServiceId` values
    /// via `ServiceId::new()`.
    #[must_use]
    pub fn with_service(mut self, service: ServiceDescription) -> Self {
        self.extra_services.push(service);
        self
    }

    /// Add multiple manually constructed `ServiceDescription` entries at once.
    #[must_use]
    pub fn with_services(mut self, services: Vec<ServiceDescription>) -> Self {
        self.extra_services.extend(services);
        self
    }

    /// **[Simulation Only]** Inject pre-filled `DaemonResources` into the daemon.
    ///
    /// This allows `MockContext` to pre-populate shelf data, status plane entries,
    /// and other resources before the daemon starts running services.
    ///
    /// # Safety
    /// This method is gated behind the `simulation` feature to prevent misuse
    /// in production environments.
    #[cfg(feature = "simulation")]
    #[must_use]
    pub fn with_resources(mut self, resources: Arc<DaemonResources>) -> Self {
        self.resources = Some(resources);
        self
    }

    /// Link the daemon to an external `CancellationToken` for hierarchical
    /// lifecycle management.
    ///
    /// When the external token is cancelled, the daemon will treat it as a
    /// shutdown signal and begin graceful termination. Conversely, when the
    /// daemon's [`shutdown()`](ServiceDaemon::shutdown) is called, it will
    /// also cancel this token, propagating the signal to all other components
    /// sharing it.
    #[must_use]
    pub fn with_cancel_token(mut self, token: CancellationToken) -> Self {
        self.external_cancel_token = Some(token);
        self
    }

    /// Register a trigger-specific configuration override.
    ///
    /// The registered config can be retrieved at runtime via
    /// [`context::trigger_config::<C>()`](crate::core::context::trigger_config).
    /// This is how users override the defaults declared by trigger templates
    /// (e.g. [`ScalingPolicy`](crate::models::ScalingPolicy)).
    ///
    /// This method can be called multiple times with different config types.
    /// Each call replaces the previous registration for that type.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let mut daemon = ServiceDaemon::builder()
    ///     .with_trigger_config(ScalingPolicy::builder()
    ///         .initial_concurrency(4)
    ///         .build())
    ///     .build();
    /// ```
    #[must_use]
    pub fn with_trigger_config<C: 'static + Clone + Send + Sync>(self, config: C) -> Self {
        self.trigger_configs
            .insert(std::any::TypeId::of::<C>(), Box::new(config));
        self
    }

    /// Build the `ServiceDaemon`.
    ///
    /// This method is **infallible** -- it always returns a valid daemon.
    /// If no registry was provided, all statically registered services are included.
    /// Any extra services added via `with_service()` are appended after registry services.
    #[must_use]
    pub fn build(self) -> ServiceDaemon {
        let registry = self.registry.unwrap_or_else(|| Registry::builder().build());
        let mut services = registry.into_services();
        services.extend(self.extra_services);

        #[cfg(feature = "simulation")]
        let resources = self.resources.unwrap_or_else(DaemonResources::new);
        #[cfg(not(feature = "simulation"))]
        let resources = DaemonResources::new();

        // Inject user-registered trigger configs into the shared resources.
        for entry in self.trigger_configs {
            resources.trigger_configs.insert(entry.0, entry.1);
        }

        ServiceDaemon {
            services,
            running_tasks: Arc::new(Mutex::new(HashMap::new())),
            restart_policy: self.restart_policy,
            cancellation_token: CancellationToken::new(),
            external_cancel_token: self.external_cancel_token,
            resources,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;
    use tracing::debug;

    use crate::models::ServiceFn;

    /// Helper: Create an isolated registry that filters out all auto-registered services.
    fn isolated_registry() -> Registry {
        Registry::builder().with_tag("__test_isolation__").build()
    }

    fn setup_tracing() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    #[tokio::test]
    async fn test_service_daemon_builder_default() {
        setup_tracing();
        let daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .build();
        debug!("test_service_daemon_builder_default passed");
        let _ = daemon;
    }

    #[tokio::test]
    async fn test_service_daemon_handle() {
        setup_tracing();
        let daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .build();
        let handle = daemon.handle();

        // Initially, unknown service should be Terminated
        let status = handle.get_service_status(&ServiceId(999)).await;
        assert_eq!(status, ServiceStatus::Terminated);

        // Insert a status manually and verify
        daemon
            .resources
            .status_plane
            .insert(ServiceId(1), ServiceStatus::Healthy);
        let status = handle.get_service_status(&ServiceId(1)).await;
        assert_eq!(status, ServiceStatus::Healthy);
    }

    #[tokio::test]
    async fn test_service_status_update() {
        setup_tracing();
        let daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .build();
        let handle = daemon.handle();

        // Insert status
        daemon
            .resources
            .status_plane
            .insert(ServiceId(0), ServiceStatus::Initializing);

        let status = handle.get_service_status(&ServiceId(0)).await;
        assert_eq!(status, ServiceStatus::Initializing);

        // Update status
        daemon
            .resources
            .status_plane
            .insert(ServiceId(0), ServiceStatus::Healthy);
        let status = handle.get_service_status(&ServiceId(0)).await;
        assert_eq!(status, ServiceStatus::Healthy);
    }

    #[tokio::test]
    async fn test_short_run() {
        setup_tracing();
        let run_count = Arc::new(AtomicU32::new(0));
        let run_count_clone = run_count.clone();

        let service_fn: ServiceFn = Arc::new(move |_cancel| {
            let count = run_count_clone.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                // Simulate a quick service
                tokio::time::sleep(Duration::from_millis(100)).await;
                Ok(())
            })
        });

        // Build manually with a service
        let daemon = ServiceDaemon {
            services: vec![ServiceDescription {
                id: ServiceId(0),
                name: Arc::from("counting_service"),
                run: service_fn,
                watcher: None,
                priority: 50,
                cancellation_token: CancellationToken::new(),
                tags: &[],
            }],
            running_tasks: Arc::new(Mutex::new(HashMap::new())),
            restart_policy: RestartPolicy::for_testing(),
            cancellation_token: CancellationToken::new(),
            external_cancel_token: None,
            resources: DaemonResources::new(),
        };

        let start = std::time::Instant::now();
        daemon
            .run_for_duration(Duration::from_millis(500))
            .await
            .unwrap();
        let elapsed = start.elapsed();

        // Should have run and restarted a few times
        let count = run_count.load(Ordering::SeqCst);
        assert!(
            count >= 1,
            "Service should have run at least once, got {}",
            count
        );
        assert!(
            elapsed >= Duration::from_millis(400),
            "Should have run for at least 400ms"
        );
    }
}
