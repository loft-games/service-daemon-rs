//! ServiceDaemon - the main orchestrator for managed services.
//!
//! This module is split into submodules for better organization:
//! - `policy`: Restart policy configuration.
//! - `runner`: Service spawning and lifecycle management.

mod policy;
mod runner;

use futures::future::BoxFuture;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument};

use crate::models::{
    Result as ServiceResult, SERVICE_REGISTRY, ServiceDescription, ServiceFn, ServiceStatus,
};
use crate::utils::context::DaemonResources;

pub use policy::{RestartPolicy, RestartPolicyBuilder};

/// A handle to the ServiceDaemon that can be used to query status and interact with services.
#[derive(Clone)]
pub struct ServiceDaemonHandle {
    resources: DaemonResources,
}

impl ServiceDaemonHandle {
    /// Get the current status of a service by name.
    pub async fn get_service_status(&self, name: &str) -> ServiceStatus {
        self.resources
            .status_plane
            .get(name)
            .map(|s| s.clone())
            .unwrap_or(ServiceStatus::Terminated)
    }
}

pub struct ServiceDaemon {
    services: Vec<ServiceDescription>,
    running_tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    restart_policy: RestartPolicy,
    cancellation_token: CancellationToken,
    /// Instance-owned resources (Status Plane, Shelf, Signals)
    resources: DaemonResources,
}

impl Default for ServiceDaemon {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceDaemon {
    /// Create a new empty daemon with default restart policy.
    #[must_use]
    pub fn new() -> Self {
        Self {
            services: Vec::new(),
            running_tasks: Arc::new(Mutex::new(HashMap::new())),
            restart_policy: RestartPolicy::default(),
            cancellation_token: CancellationToken::new(),
            resources: DaemonResources::new(),
        }
    }

    /// Create a new daemon with a custom restart policy.
    #[must_use]
    pub fn with_restart_policy(restart_policy: RestartPolicy) -> Self {
        Self {
            services: Vec::new(),
            running_tasks: Arc::new(Mutex::new(HashMap::new())),
            restart_policy,
            cancellation_token: CancellationToken::new(),
            resources: DaemonResources::new(),
        }
    }

    /// Automatically initialize the daemon by registering all auto-registered Services.
    ///
    /// Note: With Type-Based DI, providers are resolved lazily via `Provided::resolve()`
    /// when services first request them. No explicit provider initialization is needed.
    #[must_use]
    pub fn auto_init() -> Self {
        // Load services - providers are resolved lazily via Provided::resolve()
        Self::from_registry()
    }

    /// Create a new daemon with all services from the auto-generated registry.
    /// Services register themselves via the #[service] macro using linkme.
    /// NOTE: This does NOT initialize providers. Use auto_init() for full setup.
    #[must_use]
    pub fn from_registry() -> Self {
        Self::from_registry_with_policy(RestartPolicy::default())
    }

    /// Create a new daemon from registry with custom restart policy.
    #[must_use]
    pub fn from_registry_with_policy(restart_policy: RestartPolicy) -> Self {
        let mut daemon = Self::with_restart_policy(restart_policy);

        for entry in SERVICE_REGISTRY {
            info!(
                "Registering service '{}' from module '{}' with priority {} and {} params: {:?}",
                entry.name,
                entry.module,
                entry.priority,
                entry.params.len(),
                entry
                    .params
                    .iter()
                    .map(|p| p.container_key())
                    .collect::<Vec<_>>()
            );

            let wrapper = entry.wrapper;
            let watcher_ptr = entry.watcher;
            daemon.register_with_watcher(
                entry.name,
                Arc::new(wrapper),
                watcher_ptr.map(|w| Arc::new(w) as _),
                entry.priority,
            );
        }

        daemon
    }

    /// Register a service manually.
    pub fn register(&mut self, name: &str, run: ServiceFn, priority: u8) {
        self.register_with_watcher(name, run, None, priority);
    }

    /// Register a service with an optional watcher for dependency reloads.
    pub fn register_with_watcher(
        &mut self,
        name: &str,
        run: ServiceFn,
        watcher: Option<Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>>,
        priority: u8,
    ) {
        self.services.push(ServiceDescription {
            name: name.to_string(),
            run,
            watcher,
            priority,
            cancellation_token: CancellationToken::new(),
        });
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

    /// Get the current status of a service by name.
    pub async fn get_service_status(&self, name: &str) -> ServiceStatus {
        self.handle().get_service_status(name).await
    }

    /// Run the daemon until interrupted by Ctrl+C (SIGINT) or SIGTERM.
    ///
    /// This method spawns all registered services and waits for a shutdown signal.
    /// Services are automatically restarted on failure using exponential backoff.
    #[instrument(skip(self))]
    pub async fn run(self) -> ServiceResult<()> {
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
                service.name.clone(),
                service.run.clone(),
                service.watcher.clone(),
                test_policy,
                self.running_tasks.clone(),
                self.resources.clone(),
                self.cancellation_token.clone(),
            )
            .await;
        }

        tokio::time::sleep(duration).await;

        runner::stop_all_services(
            &self.services,
            self.running_tasks.clone(),
            self.resources.clone(),
            self.cancellation_token.clone(),
        )
        .await;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;
    use tracing::debug;

    fn setup_tracing() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    #[tokio::test]
    async fn test_service_daemon_new() {
        setup_tracing();
        let daemon = ServiceDaemon::new();
        assert!(daemon.services.is_empty());
        debug!("test_service_daemon_new passed");
    }

    #[tokio::test]
    async fn test_service_daemon_register() {
        setup_tracing();
        let mut daemon = ServiceDaemon::new();

        let run_count = Arc::new(AtomicU32::new(0));
        let run_count_clone = run_count.clone();

        let service_fn: ServiceFn = Arc::new(move |_cancel| {
            let count = run_count_clone.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        });

        daemon.register("test_service", service_fn, 50);

        assert_eq!(daemon.services.len(), 1);
        assert_eq!(daemon.services[0].name, "test_service");
        debug!("test_service_daemon_register passed");
    }

    #[tokio::test]
    async fn test_service_daemon_handle() {
        setup_tracing();
        let daemon = ServiceDaemon::new();
        let handle = daemon.handle();

        // Initially, unknown service should be Terminated
        let status = handle.get_service_status("unknown").await;
        assert_eq!(status, ServiceStatus::Terminated);

        // Insert a status manually and verify
        daemon
            .resources
            .status_plane
            .insert("test_svc".to_string(), ServiceStatus::Healthy);
        let status = handle.get_service_status("test_svc").await;
        assert_eq!(status, ServiceStatus::Healthy);
    }

    #[tokio::test]
    async fn test_service_status_update() {
        setup_tracing();
        let daemon = ServiceDaemon::new();
        let handle = daemon.handle();

        // Insert status
        daemon
            .resources
            .status_plane
            .insert("my_service".to_string(), ServiceStatus::Initializing);

        let status = handle.get_service_status("my_service").await;
        assert_eq!(status, ServiceStatus::Initializing);

        // Update status
        daemon
            .resources
            .status_plane
            .insert("my_service".to_string(), ServiceStatus::Healthy);
        let status = handle.get_service_status("my_service").await;
        assert_eq!(status, ServiceStatus::Healthy);
    }

    #[tokio::test]
    async fn test_short_run() {
        setup_tracing();
        let mut daemon = ServiceDaemon::with_restart_policy(RestartPolicy::for_testing());

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

        daemon.register("counting_service", service_fn, 50);

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
