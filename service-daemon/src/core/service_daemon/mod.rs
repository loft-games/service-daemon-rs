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
// ServiceDaemonHandle — lightweight status query interface
// ---------------------------------------------------------------------------

/// A handle to the ServiceDaemon that can be used to query status and interact with services.
#[derive(Clone)]
pub struct ServiceDaemonHandle {
    resources: DaemonResources,
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
// ServiceDaemon — Infallible Builder pattern
// ---------------------------------------------------------------------------

/// The main orchestrator for managed services.
///
/// Constructed via `ServiceDaemon::builder()`, which provides an **infallible**
/// `.build()` method that always succeeds.
///
/// # Examples
/// ```rust,ignore
/// // Full startup (all registered services)
/// ServiceDaemon::builder().build().run().await?;
///
/// // Tag-based startup
/// let reg = Registry::builder().with_tag("infra").build();
/// ServiceDaemon::builder().with_registry(reg).build().run().await?;
/// ```
pub struct ServiceDaemon {
    services: Vec<ServiceDescription>,
    running_tasks: Arc<Mutex<HashMap<ServiceId, JoinHandle<()>>>>,
    restart_policy: RestartPolicy,
    cancellation_token: CancellationToken,
    /// Instance-owned resources (Status Plane, Shelf, Signals)
    resources: DaemonResources,
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
    pub fn resources(&self) -> DaemonResources {
        self.resources.clone()
    }

    /// Get the current status of a service by its `ServiceId`.
    pub async fn get_service_status(&self, id: &ServiceId) -> ServiceStatus {
        self.handle().get_service_status(id).await
    }

    /// Run the daemon until interrupted by Ctrl+C (SIGINT) or SIGTERM.
    ///
    /// This method spawns all registered services and waits for a shutdown signal.
    /// Services are automatically restarted on failure using exponential backoff.
    ///
    /// # Signal Guard (Layer 1 Defense)
    /// If signal handler registration fails (e.g. restricted container environment),
    /// this method returns `Err` immediately to prevent an uncontrollable daemon.
    #[instrument(skip(self))]
    pub async fn run(self) -> ServiceResult<()> {
        if self.services.is_empty() {
            info!(
                "ServiceDaemon has no services to run. Entering idle mode, waiting for shutdown signal..."
            );
        }

        // Spawn all services
        runner::spawn_all_services(
            &self.services,
            self.restart_policy,
            self.running_tasks.clone(),
            self.resources.clone(),
        )
        .await;

        info!(
            "ServiceDaemon running with {} service(s). Press Ctrl+C to stop.",
            self.services.len()
        );

        // Wait for shutdown signal (Ctrl+C or SIGTERM)
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
            }
        }

        // Graceful shutdown
        runner::stop_all_services(
            &self.services,
            self.running_tasks.clone(),
            self.resources.clone(),
            self.cancellation_token.clone(),
            self.restart_policy.wave_stop_timeout,
        )
        .await;
        info!("ServiceDaemon stopped.");

        Ok(())
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
// ServiceDaemonBuilder — Infallible, zero-config default
// ---------------------------------------------------------------------------

/// Builder for constructing a `ServiceDaemon`.
///
/// The `.build()` method is **infallible** — it always returns a valid daemon.
pub struct ServiceDaemonBuilder {
    registry: Option<Registry>,
    restart_policy: RestartPolicy,
    extra_services: Vec<ServiceDescription>,
    /// Pre-filled resources for simulation (only available with `simulation` feature).
    #[cfg(feature = "simulation")]
    resources: Option<DaemonResources>,
}

impl ServiceDaemonBuilder {
    fn new() -> Self {
        Self {
            registry: None,
            restart_policy: RestartPolicy::default(),
            extra_services: Vec::new(),
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
    pub fn with_resources(mut self, resources: DaemonResources) -> Self {
        self.resources = Some(resources);
        self
    }

    /// Build the `ServiceDaemon`.
    ///
    /// This method is **infallible** — it always returns a valid daemon.
    /// If no registry was provided, all statically registered services are included.
    /// Any extra services added via `with_service()` are appended after registry services.
    #[must_use]
    pub fn build(self) -> ServiceDaemon {
        let registry = self.registry.unwrap_or_else(|| Registry::builder().build());
        let mut services = registry.into_services();
        services.extend(self.extra_services);

        // Use injected resources if provided (simulation), otherwise create fresh ones.
        #[cfg(feature = "simulation")]
        let resources = self.resources.unwrap_or_default();
        #[cfg(not(feature = "simulation"))]
        let resources = DaemonResources::new();

        ServiceDaemon {
            services,
            running_tasks: Arc::new(Mutex::new(HashMap::new())),
            restart_policy: self.restart_policy,
            cancellation_token: CancellationToken::new(),
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
                name: "counting_service".to_string(),
                run: service_fn,
                watcher: None,
                priority: 50,
                cancellation_token: CancellationToken::new(),
                tags: vec![],
            }],
            running_tasks: Arc::new(Mutex::new(HashMap::new())),
            restart_policy: RestartPolicy::for_testing(),
            cancellation_token: CancellationToken::new(),
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
