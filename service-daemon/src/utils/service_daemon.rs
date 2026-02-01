use dashmap::DashMap;
use futures::future::BoxFuture;
use rand::Rng;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, error, info, instrument, warn};

use crate::models::{Result as ServiceResult, SERVICE_REGISTRY, ServiceDescription, ServiceFn};
use crate::utils::context::{
    CURRENT_SERVICE, RELOAD_SIGNALS, SERVICE_STATE_STORE, ServiceIdentity, ServiceState,
};
use futures::FutureExt;
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
    /// Jitter factor (0.0 to 1.0) - randomizes delay to prevent thundering herd (default: 0.1)
    pub jitter_factor: f64,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(300), // 5 minutes
            multiplier: 2.0,
            reset_after: Duration::from_secs(60),
            jitter_factor: 0.1, // 10% jitter by default
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
            jitter_factor: 0.0, // No jitter for predictable tests
        }
    }

    /// Calculate the next restart delay using exponential backoff with jitter.
    pub fn next_delay(&self, current_delay: Duration) -> Duration {
        let base = current_delay.as_secs_f64() * self.multiplier;
        let jitter_range = base * self.jitter_factor;
        let jitter = rand::thread_rng().gen_range(-jitter_range..=jitter_range);
        let next = Duration::from_secs_f64((base + jitter).max(0.0));
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

    pub fn jitter_factor(mut self, factor: f64) -> Self {
        self.policy.jitter_factor = factor.clamp(0.0, 1.0);
        self
    }

    #[must_use]
    pub fn build(self) -> RestartPolicy {
        self.policy
    }
}

/// A handle to the ServiceDaemon that can be used to query status and interact with services.
#[derive(Clone)]
pub struct ServiceDaemonHandle {
    service_status: Arc<DashMap<String, ServiceStatus>>,
}

impl ServiceDaemonHandle {
    /// Get the current status of a service by name.
    pub async fn get_service_status(&self, name: &str) -> ServiceStatus {
        self.service_status
            .get(name)
            .map(|s| *s)
            .unwrap_or(ServiceStatus::Stopped)
    }
}

pub struct ServiceDaemon {
    services: Vec<ServiceDescription>,
    running_tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    service_status: Arc<DashMap<String, ServiceStatus>>,
    restart_policy: RestartPolicy,
    cancellation_token: CancellationToken,
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
            service_status: Arc::new(DashMap::new()),
            restart_policy: RestartPolicy::default(),
            cancellation_token: CancellationToken::new(),
        }
    }

    /// Create a new daemon with a custom restart policy.
    #[must_use]
    pub fn with_restart_policy(restart_policy: RestartPolicy) -> Self {
        Self {
            services: Vec::new(),
            running_tasks: Arc::new(Mutex::new(HashMap::new())),
            service_status: Arc::new(DashMap::new()),
            restart_policy,
            cancellation_token: CancellationToken::new(),
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
        watcher: Option<Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>>,
        policy: RestartPolicy,
        running_tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
        service_status: Arc<DashMap<String, ServiceStatus>>,
        cancellation_token: CancellationToken,
    ) {
        let name_for_task = name.clone();
        let handle = tokio::spawn(async move {
            let mut current_delay = policy.initial_delay;
            let name = name_for_task;

            // Spawn dependency watcher if present
            if let Some(watcher) = watcher {
                let n = name.clone();
                let ct = cancellation_token.clone();
                tokio::spawn(async move {
                    while !ct.is_cancelled() {
                        let reload_signal = RELOAD_SIGNALS
                            .entry(n.clone())
                            .or_insert_with(|| Arc::new(tokio::sync::Notify::new()))
                            .clone();

                        tokio::select! {
                            _ = watcher() => {
                                info!("Watcher: Dependency change detected for service '{}', triggering reload", n);
                                reload_signal.notify_one();
                            }
                            _ = ct.cancelled() => break,
                        }
                    }
                });
            }

            loop {
                // Check for cancellation before starting
                // Determine initial state
                let initial_state = if let Some(state) = SERVICE_STATE_STORE.get(&name) {
                    state.value().clone()
                } else {
                    ServiceState::Starting
                };

                // Normalizing initial state: if we were reloading, the new instance starts as Restoring.
                // If we were starting or recovering, we preserve that state.
                let start_state = match initial_state {
                    ServiceState::Starting => ServiceState::Starting,
                    ServiceState::Recovering(e) => ServiceState::Recovering(e),
                    _ => ServiceState::Restoring,
                };

                if cancellation_token.is_cancelled() {
                    info!(
                        "Service {} received shutdown signal, exiting gracefully",
                        name
                    );
                    service_status.insert(name.clone(), ServiceStatus::Stopped);
                    break;
                }

                info!("Starting service: {} with state {:?}", name, start_state);
                service_status.insert(name.clone(), ServiceStatus::Running);
                let start_time = std::time::Instant::now();

                let span = tracing::info_span!("service", %name);
                let token_clone = cancellation_token.clone();

                // Get or create reload signal
                let reload_signal = RELOAD_SIGNALS
                    .entry(name.clone())
                    .or_insert_with(|| Arc::new(tokio::sync::Notify::new()))
                    .clone();

                // Create a reload token that we can cancel when a reload is requested.
                // This allows state().await to return NeedReload immediately.
                let reload_token = CancellationToken::new();
                let rt_clone = reload_token.clone();
                let rs_clone = reload_signal.clone();
                let n = name.clone();
                let bridge_task = tokio::spawn(async move {
                    rs_clone.notified().await;
                    rt_clone.cancel();
                    info!(
                        "Supervisor: Reload triggered for {}, notifying service task",
                        n
                    );
                });

                let identity = ServiceIdentity {
                    name: name.clone(),
                    reload_signal: reload_signal.clone(),
                    cancellation_token: token_clone.clone(),
                    reload_token: reload_token.clone(),
                };

                // Update current state in store
                SERVICE_STATE_STORE.insert(name.clone(), start_state);

                // Wrapper to run service in TLS scope and capture errors
                let run_clone = run.clone();
                let token_for_run = token_clone.clone();

                let result = CURRENT_SERVICE
                    .scope(identity, async move {
                        std::panic::AssertUnwindSafe(run_clone(token_for_run).instrument(span))
                            .catch_unwind()
                            .await
                    })
                    .await;

                bridge_task.abort();

                let mut next_state = ServiceState::Starting;

                match result {
                    Ok(Ok(_)) => {
                        warn!("Service {} exited normally", name);
                    }
                    Ok(Err(e)) => {
                        error!("Service {} failed: {:?}", name, e);
                        next_state = ServiceState::Recovering(format!("{:?}", e));
                    }
                    Err(panic) => {
                        let panic_msg = if let Some(s) = panic.downcast_ref::<&str>() {
                            s.to_string()
                        } else if let Some(s) = panic.downcast_ref::<String>() {
                            s.clone()
                        } else {
                            "Unknown panic".to_string()
                        };
                        error!("Service {} panicked: {}", name, panic_msg);
                        next_state = ServiceState::Recovering(format!("Panic: {}", panic_msg));
                    }
                };

                // Check for explicit reload or normal exit
                if reload_token.is_cancelled() {
                    info!("Supervisor: Service {} exited after reload signal", name);
                    next_state = ServiceState::Restoring;
                }

                // Check for cancellation after service exits
                if cancellation_token.is_cancelled() {
                    info!("Service {} received shutdown signal, not restarting", name);
                    service_status.insert(name.clone(), ServiceStatus::Stopped);
                    SERVICE_STATE_STORE.remove(&name);
                    break;
                }

                service_status.insert(name.clone(), ServiceStatus::Restarting);
                info!(
                    "Supervisor: Setting next_state for {} to {:?}",
                    name, next_state
                );
                SERVICE_STATE_STORE.insert(name.clone(), next_state);

                // Reset delay if service ran successfully for long enough
                if start_time.elapsed() >= policy.reset_after {
                    current_delay = policy.initial_delay;
                }

                warn!(
                    "Restarting service {} in {:.1}s...",
                    name,
                    current_delay.as_secs_f64()
                );

                // Use select to allow cancellation during sleep
                tokio::select! {
                    _ = tokio::time::sleep(current_delay) => {}
                    _ = cancellation_token.cancelled() => {
                        info!("Service {} received shutdown signal during restart delay", name);
                        service_status.insert(name.clone(), ServiceStatus::Stopped);
                        break;
                    }
                }

                // Apply exponential backoff for next restart
                current_delay = policy.next_delay(current_delay);
            }
        });

        running_tasks.lock().await.insert(name, handle);
    }

    /// Spawn all registered services using wave-based priorities.
    ///
    /// This starts services in descending order of their `priority` value.
    /// Services with high priority (e.g. SYSTEM = 100) start first.
    async fn spawn_all_services(&self) {
        use std::collections::BTreeMap;

        info!("Beginning wave-based startup sequence...");

        // Group services by priority
        let mut waves: BTreeMap<u8, Vec<&ServiceDescription>> = BTreeMap::new();
        for service in &self.services {
            waves.entry(service.priority).or_default().push(service);
        }

        // Process waves in descending order of priority (u8)
        for (priority, services) in waves.into_iter().rev() {
            info!(
                "Starting wave priority {} ({} services)...",
                priority,
                services.len()
            );

            for service in &services {
                Self::spawn_service(
                    service.name.clone(),
                    service.run.clone(),
                    service.watcher.clone(),
                    self.restart_policy,
                    self.running_tasks.clone(),
                    self.service_status.clone(),
                    service.cancellation_token.clone(),
                )
                .await;
            }

            // Sync Step: Wait for all services in this wave to be reported as 'Running'
            // This ensures Wave 100 actually initialized before Wave 80 starts.
            let start = std::time::Instant::now();
            let mut all_running = false;
            while !all_running && start.elapsed() < std::time::Duration::from_secs(5) {
                all_running = true;
                for service in &services {
                    let status = self.service_status.get(&service.name).map(|r| *r.value());
                    if status != Some(ServiceStatus::Running) {
                        all_running = false;
                        break;
                    }
                }
                if !all_running {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }

            if !all_running {
                warn!(
                    "Wave priority {} did not reach 'Running' status within 5s, proceeding anyway",
                    priority
                );
            }
        }

        info!("All startup waves initiated.");
    }

    /// Stop all running services gracefully using wave-based priorities.
    ///
    /// This stops services in ascending order of their `priority` value.
    /// Services with the same priority are shut down concurrently.
    async fn stop_all_services(&self) {
        use std::collections::BTreeMap;

        info!("Beginning wave-based graceful shutdown...");

        // Group services by priority
        let mut waves: BTreeMap<u8, Vec<&ServiceDescription>> = BTreeMap::new();
        for service in &self.services {
            waves.entry(service.priority).or_default().push(service);
        }

        let grace_period = std::time::Duration::from_secs(30);

        // Process waves in ascending order of priority (u8)
        for (priority, services) in waves {
            info!(
                "Shutting down wave priority {} ({} services)...",
                priority,
                services.len()
            );

            // 1. Parallel Signal: Cancel all services in this wave
            for service in &services {
                service.cancellation_token.cancel();
                SERVICE_STATE_STORE.insert(service.name.clone(), ServiceState::Stopping);
            }

            // 2. Parallel Wait: Wait for all services in this wave to finish
            let mut join_handles = Vec::new();
            for service in services {
                let name = &service.name;
                let handle_opt = {
                    let mut guard = self.running_tasks.lock().await;
                    guard.remove(name)
                };
                if let Some(handle) = handle_opt {
                    join_handles.push((name.clone(), handle));
                }
            }

            let mut shutdown_futures = Vec::new();
            for (name, mut handle) in join_handles {
                let status_clone = self.service_status.clone();
                shutdown_futures.push(async move {
                    info!("Waiting for service '{}' to stop...", name);
                    tokio::select! {
                        res = &mut handle => {
                            match res {
                                Ok(()) => info!("Service '{}' stopped gracefully", name),
                                Err(e) => warn!("Service '{}' panicked during shutdown: {:?}", name, e),
                            }
                        }
                        _ = tokio::time::sleep(grace_period) => {
                            warn!(
                                "Service '{}' did not stop within grace period, forcing abort",
                                name
                            );
                            handle.abort();
                            let _ = handle.await;
                        }
                    }
                    status_clone.insert(name, ServiceStatus::Stopped);
                });
            }
            futures::future::join_all(shutdown_futures).await;
        }

        // Finally, cancel the daemon's own token to signal completion if anyone is watching it
        self.cancellation_token.cancel();
        info!("All shutdown waves completed. ServiceDaemon stopped.");
    }

    /// Run the daemon until interrupted by Ctrl+C (SIGINT) or SIGTERM.
    ///
    /// This method spawns all registered services and waits for a shutdown signal.
    /// Services are automatically restarted on failure using exponential backoff.
    #[instrument(skip(self))]
    pub async fn run(self) -> ServiceResult<()> {
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
        self.stop_all_services().await;
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
            Self::spawn_service(
                service.name.clone(),
                service.run.clone(),
                service.watcher.clone(),
                test_policy,
                self.running_tasks.clone(),
                self.service_status.clone(),
                self.cancellation_token.clone(),
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

        let service_fn: ServiceFn = Arc::new(|_| Box::pin(async { Ok(()) }));

        daemon.register("test_service", service_fn, 50);

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

        let service_fn: ServiceFn = Arc::new(move |_| {
            let count = call_count_clone.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                Err(anyhow::anyhow!("Intentional failure"))
            })
        });

        daemon.register("failing_service", service_fn, 50);

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
    async fn test_crash_recovery_state() {
        setup_tracing();
        let mut daemon = ServiceDaemon::new();

        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = call_count.clone();

        let service_fn: ServiceFn = Arc::new(move |_| {
            let count = call_count_clone.clone();
            Box::pin(async move {
                let c = count.fetch_add(1, Ordering::SeqCst);
                if c == 0 {
                    panic!("Boom!");
                } else {
                    let s = crate::utils::context::state();
                    info!("Service received state: {:?}", s);
                    if let ServiceState::Recovering(ref err) = s {
                        if err.contains("Boom") {
                            info!("Successfully detected recovery state with message: {}", err);
                            while !crate::is_shutdown() {
                                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                            }
                            return Ok(());
                        }
                    }
                    Err(anyhow::anyhow!(
                        "Expected Recovering state with Boom!, got {:?}",
                        s
                    ))
                }
            })
        });

        daemon.register("crash_recovery_test_service", service_fn, 50);

        daemon
            .run_for_duration(Duration::from_secs(3))
            .await
            .unwrap();

        let final_count = call_count.load(Ordering::SeqCst);
        assert!(final_count >= 2);
    }

    #[tokio::test]
    async fn test_service_runs_successfully() {
        setup_tracing();
        let mut daemon = ServiceDaemon::new();

        let executed = Arc::new(AtomicU32::new(0));
        let executed_clone = executed.clone();

        let service_fn: ServiceFn = Arc::new(move |_| {
            let exec = executed_clone.clone();
            Box::pin(async move {
                exec.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(100)).await;
                Ok(())
            })
        });

        daemon.register("successful_service", service_fn, 50);

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
        let service_a: ServiceFn = Arc::new(move |_| {
            let count = count_a_clone.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(())
            })
        });

        let count_b_clone = count_b.clone();
        let service_b: ServiceFn = Arc::new(move |_| {
            let count = count_b_clone.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(())
            })
        });

        daemon.register("service_a", service_a, 50);
        daemon.register("service_b", service_b, 50);

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

    #[tokio::test]
    async fn test_state_handoff_shelving() {
        setup_tracing();
        let mut daemon = ServiceDaemon::new();

        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = call_count.clone();

        let service_fn: ServiceFn = Arc::new(move |_| {
            let count = call_count_clone.clone();
            Box::pin(async move {
                let c = count.fetch_add(1, Ordering::SeqCst);
                if c == 0 {
                    info!("First run: shelving data");
                    crate::utils::context::shelve("data", "Hello Shelf".to_string()).await;
                    // Simulate an error to trigger restart
                    return Err(anyhow::anyhow!("Restart me"));
                } else {
                    info!("Second run: checking shelf");
                    let data: Option<String> = crate::utils::context::unshelve("data").await;
                    if let Some(ref msg) = data {
                        if msg == "Hello Shelf" {
                            info!("Successfully unshelved data!");
                            // Wait for shutdown to prevent restarting and failing on subsequently empty shelf
                            while !crate::is_shutdown() {
                                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                            }
                            return Ok(());
                        }
                    }
                    Err(anyhow::anyhow!(
                        "Expected 'Hello Shelf' in shelf, got {:?}",
                        data
                    ))
                }
            })
        });

        daemon.register("shelve_service", service_fn, 50);

        daemon
            .run_for_duration(Duration::from_secs(3))
            .await
            .unwrap();

        let final_count = call_count.load(Ordering::SeqCst);
        assert!(final_count >= 2);
    }
}
