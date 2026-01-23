use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{error, info, instrument, warn};

use crate::models::{SERVICE_REGISTRY, ServiceDescription, ServiceFn};

/// Current status of a service managed by the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceStatus {
    /// Service is currently running.
    Running,
    /// Service has failed and is waiting to be restarted.
    Restarting,
    /// Service has been stopped gracefully or never started.
    Stopped,
}

/// Configuration for service restart behavior with exponential backoff.
#[derive(Debug, Clone, Copy)]
pub struct RestartPolicy {
    /// Initial delay before first restart (default: 1 second)
    pub initial_delay: Duration,
    /// Maximum delay between restarts (default: 5 minutes)
    pub max_delay: Duration,
    /// Multiplier for exponential backoff (default: 2.0)
    pub multiplier: f64,
    /// Delay resets to initial after this duration of successful running (default: 60 seconds)
    pub reset_after: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(300), // 5 minutes
            multiplier: 2.0,
            reset_after: Duration::from_secs(60),
        }
    }
}

impl RestartPolicy {
    /// Create a restart policy builder.
    pub fn builder() -> RestartPolicyBuilder {
        RestartPolicyBuilder::default()
    }

    /// Create a restart policy for testing with shorter delays.
    pub fn for_testing() -> Self {
        Self {
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(2),
            multiplier: 2.0,
            reset_after: Duration::from_secs(5),
        }
    }

    /// Calculate the next restart delay using exponential backoff.
    pub fn next_delay(&self, current_delay: Duration) -> Duration {
        let next = Duration::from_secs_f64(current_delay.as_secs_f64() * self.multiplier);
        next.min(self.max_delay)
    }
}

/// Builder for `RestartPolicy`.
#[derive(Default)]
pub struct RestartPolicyBuilder {
    policy: RestartPolicy,
}

impl RestartPolicyBuilder {
    pub fn initial_delay(mut self, delay: Duration) -> Self {
        self.policy.initial_delay = delay;
        self
    }

    pub fn max_delay(mut self, delay: Duration) -> Self {
        self.policy.max_delay = delay;
        self
    }

    pub fn multiplier(mut self, multiplier: f64) -> Self {
        self.policy.multiplier = multiplier;
        self
    }

    pub fn reset_after(mut self, duration: Duration) -> Self {
        self.policy.reset_after = duration;
        self
    }

    pub fn build(self) -> RestartPolicy {
        self.policy
    }
}

/// A handle to the ServiceDaemon that can be used to query status and interact with services.
#[derive(Clone)]
pub struct ServiceDaemonHandle {
    service_status: Arc<Mutex<HashMap<String, ServiceStatus>>>,
}

impl ServiceDaemonHandle {
    /// Get the current status of a service by name.
    pub async fn get_service_status(&self, name: &str) -> ServiceStatus {
        self.service_status
            .lock()
            .await
            .get(name)
            .copied()
            .unwrap_or(ServiceStatus::Stopped)
    }
}

/// A daemon that manages long-running services with automatic restart and DI support.
pub struct ServiceDaemon {
    services: Vec<ServiceDescription>,
    running_tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    service_status: Arc<Mutex<HashMap<String, ServiceStatus>>>,
    restart_policy: RestartPolicy,
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
            service_status: Arc::new(Mutex::new(HashMap::new())),
            restart_policy: RestartPolicy::default(),
        }
    }

    /// Create a new daemon with a custom restart policy.
    #[must_use]
    pub fn with_restart_policy(restart_policy: RestartPolicy) -> Self {
        Self {
            services: Vec::new(),
            running_tasks: Arc::new(Mutex::new(HashMap::new())),
            service_status: Arc::new(Mutex::new(HashMap::new())),
            restart_policy,
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
                "Registering service '{}' from module '{}' with {} params: {:?}",
                entry.name,
                entry.module,
                entry.params.len(),
                entry
                    .params
                    .iter()
                    .map(|p| p.container_key())
                    .collect::<Vec<_>>()
            );

            let wrapper = entry.wrapper;
            daemon.register(entry.name, Arc::new(move || wrapper()));
        }

        daemon
    }

    /// Register a service manually.
    pub fn register(&mut self, name: &str, run: ServiceFn) {
        self.services.push(ServiceDescription {
            name: name.to_string(),
            run,
        });
    }

    /// Get a handle to the daemon for querying status.
    pub fn handle(&self) -> ServiceDaemonHandle {
        ServiceDaemonHandle {
            service_status: self.service_status.clone(),
        }
    }

    /// Get the current status of a service by name.
    pub async fn get_service_status(&self, name: &str) -> ServiceStatus {
        self.handle().get_service_status(name).await
    }

    /// Spawn a single service with the given restart policy.
    async fn spawn_service(
        name: String,
        run: ServiceFn,
        policy: RestartPolicy,
        running_tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
        service_status: Arc<Mutex<HashMap<String, ServiceStatus>>>,
    ) {
        let name_for_task = name.clone();
        let handle = tokio::spawn(async move {
            let mut current_delay = policy.initial_delay;
            let name = name_for_task;

            loop {
                info!("Starting service: {}", name);
                service_status
                    .lock()
                    .await
                    .insert(name.clone(), ServiceStatus::Running);
                let start_time = std::time::Instant::now();

                match run().await {
                    Ok(_) => warn!("Service {} exited normally", name),
                    Err(e) => error!("Service {} failed: {:?}", name, e),
                }

                service_status
                    .lock()
                    .await
                    .insert(name.clone(), ServiceStatus::Restarting);

                // Reset delay if service ran successfully for long enough
                if start_time.elapsed() >= policy.reset_after {
                    current_delay = policy.initial_delay;
                }

                warn!(
                    "Restarting service {} in {:.1}s...",
                    name,
                    current_delay.as_secs_f64()
                );
                tokio::time::sleep(current_delay).await;

                // Apply exponential backoff for next restart
                current_delay = policy.next_delay(current_delay);
            }
        });

        running_tasks.lock().await.insert(name, handle);
    }

    /// Spawn all registered services.
    async fn spawn_all_services(&self) {
        for service in &self.services {
            Self::spawn_service(
                service.name.clone(),
                service.run.clone(),
                self.restart_policy,
                self.running_tasks.clone(),
                self.service_status.clone(),
            )
            .await;
        }
    }

    /// Stop all running services gracefully.
    async fn stop_all_services(&self) {
        let mut tasks = self.running_tasks.lock().await;
        let mut status = self.service_status.lock().await;
        for (name, handle) in tasks.drain() {
            info!("Stopping service: {}", name);
            status.insert(name, ServiceStatus::Stopped);
            handle.abort();
        }
    }

    /// Run the daemon until interrupted by Ctrl+C (SIGINT) or SIGTERM.
    ///
    /// This method spawns all registered services and waits for a shutdown signal.
    /// Services are automatically restarted on failure using exponential backoff.
    #[instrument(skip(self))]
    pub async fn run(self) -> anyhow::Result<()> {
        // Spawn all services
        self.spawn_all_services().await;

        info!(
            "ServiceDaemon running with {} service(s). Press Ctrl+C to stop.",
            self.services.len()
        );

        // Wait for shutdown signal (Ctrl+C or SIGTERM)
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let mut sigint = signal(SignalKind::interrupt())?;
            let mut sigterm = signal(SignalKind::terminate())?;

            tokio::select! {
                _ = sigint.recv() => {
                    info!("Received SIGINT, shutting down...");
                }
                _ = sigterm.recv() => {
                    info!("Received SIGTERM, shutting down...");
                }
            }
        }

        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c().await?;
            info!("Received Ctrl+C, shutting down...");
        }

        // Graceful shutdown
        self.stop_all_services().await;
        info!("ServiceDaemon stopped.");

        Ok(())
    }

    /// Run for a limited duration (for testing).
    #[allow(dead_code)]
    #[instrument(skip(self))]
    pub async fn run_for_duration(self, duration: Duration) -> anyhow::Result<()> {
        // Use testing policy with shorter delays
        let test_policy = RestartPolicy::for_testing();

        for service in &self.services {
            Self::spawn_service(
                service.name.clone(),
                service.run.clone(),
                test_policy,
                self.running_tasks.clone(),
                self.service_status.clone(),
            )
            .await;
        }

        tokio::time::sleep(duration).await;

        self.stop_all_services().await;

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

        let service_fn: ServiceFn = Arc::new(|| Box::pin(async { Ok(()) }));

        daemon.register("test_service", service_fn);

        assert_eq!(daemon.services.len(), 1);
        assert_eq!(daemon.services[0].name, "test_service");
        debug!("test_service_daemon_register passed");
    }

    #[tokio::test]
    async fn test_service_runs_and_restarts_on_error() {
        setup_tracing();
        let mut daemon = ServiceDaemon::new();

        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = call_count.clone();

        let service_fn: ServiceFn = Arc::new(move || {
            let count = call_count_clone.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                Err(anyhow::anyhow!("Intentional failure"))
            })
        });

        daemon.register("failing_service", service_fn);

        daemon
            .run_for_duration(Duration::from_secs(3))
            .await
            .unwrap();

        let final_count = call_count.load(Ordering::SeqCst);
        debug!("Service was called {} times", final_count);

        assert!(
            final_count >= 2,
            "Service should restart on failure, but only ran {} times",
            final_count
        );
        debug!("test_service_runs_and_restarts_on_error passed");
    }

    #[tokio::test]
    async fn test_service_runs_successfully() {
        setup_tracing();
        let mut daemon = ServiceDaemon::new();

        let executed = Arc::new(AtomicU32::new(0));
        let executed_clone = executed.clone();

        let service_fn: ServiceFn = Arc::new(move || {
            let exec = executed_clone.clone();
            Box::pin(async move {
                exec.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(100)).await;
                Ok(())
            })
        });

        daemon.register("successful_service", service_fn);

        daemon
            .run_for_duration(Duration::from_secs(2))
            .await
            .unwrap();

        let final_count = executed.load(Ordering::SeqCst);
        debug!("Service executed {} times", final_count);

        assert!(
            final_count >= 1,
            "Service should have executed at least once"
        );
        debug!("test_service_runs_successfully passed");
    }

    #[tokio::test]
    async fn test_multiple_services() {
        setup_tracing();
        let mut daemon = ServiceDaemon::new();

        let count_a = Arc::new(AtomicU32::new(0));
        let count_b = Arc::new(AtomicU32::new(0));

        let count_a_clone = count_a.clone();
        let service_a: ServiceFn = Arc::new(move || {
            let count = count_a_clone.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(())
            })
        });

        let count_b_clone = count_b.clone();
        let service_b: ServiceFn = Arc::new(move || {
            let count = count_b_clone.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(())
            })
        });

        daemon.register("service_a", service_a);
        daemon.register("service_b", service_b);

        daemon
            .run_for_duration(Duration::from_secs(2))
            .await
            .unwrap();

        let final_a = count_a.load(Ordering::SeqCst);
        let final_b = count_b.load(Ordering::SeqCst);

        debug!(
            "Service A executed {} times, Service B executed {} times",
            final_a, final_b
        );

        assert!(final_a >= 1, "Service A should have executed");
        assert!(final_b >= 1, "Service B should have executed");
        debug!("test_multiple_services passed");
    }
}
