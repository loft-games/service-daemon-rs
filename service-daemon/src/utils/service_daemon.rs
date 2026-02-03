use futures::future::BoxFuture;
use rand::Rng;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, error, info, instrument, warn};

use crate::models::{
    Result as ServiceResult, SERVICE_REGISTRY, ServiceDescription, ServiceFn, ServiceStatus,
};
use crate::utils::context::{CURRENT_SERVICE, DaemonResources, ServiceIdentity};
use futures::FutureExt;

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

    /// Spawn a single service with the given restart policy.
    async fn spawn_service(
        name: String,
        run: ServiceFn,
        watcher: Option<Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>>,
        policy: RestartPolicy,
        running_tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
        resources: DaemonResources,
        cancellation_token: CancellationToken,
    ) {
        let name_for_task = name.clone();
        let resources_clone = resources.clone();
        let handle = tokio::spawn(async move {
            let mut current_delay = policy.initial_delay;
            let name = name_for_task;
            let resources = resources_clone;

            // Spawn dependency watcher if present
            if let Some(watcher) = watcher {
                let n = name.clone();
                let ct = cancellation_token.clone();
                let res = resources.clone();
                tokio::spawn(async move {
                    while !ct.is_cancelled() {
                        let reload_signal = res
                            .reload_signals
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
                // Determine initial status based on the previous generation
                let initial_status = resources
                    .status_plane
                    .get(&name)
                    .map(|s| s.value().clone())
                    .unwrap_or(ServiceStatus::Initializing);

                // Normalizing initial status: if we were reloading, the new instance starts as Restoring.
                // If we were starting or recovering, we preserve that status.
                let start_status = match initial_status {
                    ServiceStatus::Initializing => ServiceStatus::Initializing,
                    ServiceStatus::Recovering(e) => ServiceStatus::Recovering(e),
                    _ => ServiceStatus::Restoring,
                };

                if cancellation_token.is_cancelled() {
                    info!(
                        "Service {} received shutdown signal, exiting gracefully",
                        name
                    );
                    resources
                        .status_plane
                        .insert(name.clone(), ServiceStatus::Terminated);
                    resources.status_changed.notify_waiters();
                    break;
                }

                info!("Starting service: {} with status {:?}", name, start_status);
                resources
                    .status_plane
                    .insert(name.clone(), start_status.clone());
                resources.status_changed.notify_waiters();
                let start_time = std::time::Instant::now();

                let span = tracing::info_span!("service", %name);
                let token_clone = cancellation_token.clone();

                // Get or create reload signal
                let reload_signal = resources
                    .reload_signals
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
                    resources: resources.clone(),
                };

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

                // Determine next status based on outcome
                // Note: Shelf is preserved for recovery; service can decide whether to use it.
                let mut next_status = ServiceStatus::Initializing;

                match result {
                    Ok(Ok(_)) => {
                        warn!("Service {} exited normally", name);
                    }
                    Ok(Err(e)) => {
                        error!("Service {} failed: {:?}", name, e);
                        next_status = ServiceStatus::Recovering(format!("{:?}", e));
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
                        next_status = ServiceStatus::Recovering(format!("Panic: {}", panic_msg));
                    }
                };

                // Check for explicit reload or normal exit
                if reload_token.is_cancelled() {
                    info!("Supervisor: Service {} exited after reload signal", name);
                    next_status = ServiceStatus::Restoring;
                }

                // Check for cancellation after service exits
                if cancellation_token.is_cancelled() {
                    info!("Service {} received shutdown signal, not restarting", name);
                    resources
                        .status_plane
                        .insert(name.clone(), ServiceStatus::Terminated);
                    resources.status_changed.notify_waiters();
                    break;
                }

                info!(
                    "Supervisor: Setting next_status for {} to {:?}",
                    name, next_status
                );
                resources.status_plane.insert(name.clone(), next_status);
                resources.status_changed.notify_waiters();

                // Reset delay if service ran successfully for long enough
                if start_time.elapsed() >= policy.reset_after {
                    current_delay = policy.initial_delay;
                }

                warn!(
                    "Restarting service {} in {:.1}s...",
                    name,
                    current_delay.as_secs_f64()
                );

                // Use select to allow cancellation OR reload during sleep
                tokio::select! {
                    _ = tokio::time::sleep(current_delay) => {}
                    _ = reload_signal.notified() => {
                        info!("Supervisor: Service {} received immediate reload during restart delay", name);
                        current_delay = Duration::from_millis(0); // Restart immediately
                    }
                    _ = cancellation_token.cancelled() => {
                        info!("Service {} received shutdown signal during restart delay", name);
                        resources.status_plane.insert(name.clone(), ServiceStatus::Terminated);
                        resources.status_changed.notify_waiters();
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
                    self.resources.clone(),
                    service.cancellation_token.clone(),
                )
                .await;
            }

            // Sync Step: Wait for all services in this wave to become 'Healthy'
            // This ensures Wave 100 actually initialized before Wave 80 starts.
            let start = std::time::Instant::now();
            let mut all_healthy = false;
            while !all_healthy && start.elapsed() < std::time::Duration::from_secs(5) {
                all_healthy = true;
                for service in &services {
                    let status = self
                        .resources
                        .status_plane
                        .get(&service.name)
                        .map(|r| r.value().clone());
                    if status != Some(ServiceStatus::Healthy) {
                        all_healthy = false;
                        break;
                    }
                }
                if !all_healthy {
                    tokio::select! {
                        _ = self.resources.status_changed.notified() => {}
                        _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
                    }
                }
            }

            if !all_healthy {
                warn!(
                    "Wave priority {} did not reach 'Healthy' status within 5s, proceeding anyway",
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
                self.resources
                    .status_plane
                    .insert(service.name.clone(), ServiceStatus::ShuttingDown);
                self.resources.status_changed.notify_waiters();
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

            let resources = self.resources.clone();
            let mut shutdown_futures = Vec::new();
            for (name, mut handle) in join_handles {
                let res = resources.clone();
                shutdown_futures.push(async move {
                    info!("Waiting for service '{}' to stop...", name);
                    tokio::select! {
                        res_join = &mut handle => {
                            match res_join {
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
                    res.status_plane.insert(name, ServiceStatus::Terminated);
                    res.status_changed.notify_waiters();
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
                self.resources.clone(),
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

        let _ = daemon.run_for_duration(Duration::from_millis(200)).await;
        assert!(run_count.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn test_restart_policy_defaults() {
        let policy = RestartPolicy::default();
        assert_eq!(policy.initial_delay, Duration::from_secs(1));
        assert_eq!(policy.max_delay, Duration::from_secs(300));
        assert_eq!(policy.multiplier, 2.0);
        assert_eq!(policy.reset_after, Duration::from_secs(60));
    }

    #[tokio::test]
    async fn test_restart_policy_builder() {
        let policy = RestartPolicy::builder()
            .initial_delay(Duration::from_millis(500))
            .max_delay(Duration::from_secs(60))
            .multiplier(1.5)
            .jitter_factor(0.2)
            .build();

        assert_eq!(policy.initial_delay, Duration::from_millis(500));
        assert_eq!(policy.max_delay, Duration::from_secs(60));
        assert_eq!(policy.multiplier, 1.5);
        assert_eq!(policy.jitter_factor, 0.2);
    }
}
