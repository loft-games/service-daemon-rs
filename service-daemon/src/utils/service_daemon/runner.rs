//! Service runner logic for spawning, supervising, and stopping services.

use futures::FutureExt;
use futures::future::BoxFuture;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, error, info, warn};

use crate::models::{ServiceDescription, ServiceError, ServiceFn, ServiceStatus};
use crate::utils::context::{__run_service_scope, DaemonResources, ServiceIdentity};

use super::policy::RestartPolicy;

/// Supervises a single service's lifecycle, including restarts and signal handling.
struct ServiceSupervisor {
    name: String,
    run: ServiceFn,
    watcher: Option<Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>>,
    policy: RestartPolicy,
    resources: DaemonResources,
    cancellation_token: CancellationToken,
    current_delay: Duration,
}

impl ServiceSupervisor {
    fn new(
        name: String,
        run: ServiceFn,
        watcher: Option<Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>>,
        policy: RestartPolicy,
        resources: DaemonResources,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self {
            name,
            run,
            watcher,
            policy,
            resources,
            cancellation_token,
            current_delay: policy.initial_delay,
        }
    }

    /// Spawns the dependency watcher if present.
    fn spawn_watcher(&self) {
        if let Some(watcher) = &self.watcher {
            let n = self.name.clone();
            let ct = self.cancellation_token.clone();
            let res = self.resources.clone();
            let watcher = watcher.clone();
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
    }

    /// Determines the initial status for a new service generation.
    fn determine_start_status(&self) -> ServiceStatus {
        let initial_status = self
            .resources
            .status_plane
            .get(&self.name)
            .map(|s| s.value().clone())
            .unwrap_or(ServiceStatus::Initializing);

        match initial_status {
            ServiceStatus::Initializing => ServiceStatus::Initializing,
            ServiceStatus::Recovering(e) => ServiceStatus::Recovering(e),
            _ => ServiceStatus::Restoring,
        }
    }

    /// Handles the outcome of a service execution.
    /// Returns `true` if the service should be restarted, `false` if it should stop permanently.
    fn handle_outcome(
        &self,
        result: Result<Result<(), anyhow::Error>, Box<dyn std::any::Any + Send>>,
        reload_token: &CancellationToken,
    ) -> (ServiceStatus, bool) {
        let mut should_restart = true;

        let next_status = match result {
            Ok(Ok(_)) => {
                warn!("Service {} exited normally", self.name);
                ServiceStatus::Initializing
            }
            Ok(Err(e)) => {
                // Check for fatal error
                if let Some(svc_err) = e.downcast_ref::<ServiceError>()
                    && matches!(svc_err, ServiceError::Fatal(_))
                {
                    error!(
                        "Service {} encountered fatal error: {:?}",
                        self.name, svc_err
                    );
                    should_restart = false;
                    return (ServiceStatus::Terminated, should_restart);
                }
                error!("Service {} failed: {:?}", self.name, e);
                ServiceStatus::Recovering(format!("{:?}", e))
            }
            Err(panic) => {
                let panic_msg = if let Some(s) = panic.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "Unknown panic".to_string()
                };
                error!("Service {} panicked: {}", self.name, panic_msg);
                ServiceStatus::Recovering(format!("Panic: {}", panic_msg))
            }
        };

        // Check for explicit reload
        if reload_token.is_cancelled() {
            info!(
                "Supervisor: Service {} exited after reload signal",
                self.name
            );
            return (ServiceStatus::Restoring, true);
        }

        (next_status, should_restart)
    }

    /// Waits for the restart delay, allowing early exit on reload or cancellation.
    /// Returns `true` if restart should proceed, `false` if shutdown was requested.
    async fn wait_for_restart(&mut self) -> bool {
        let reload_signal = self
            .resources
            .reload_signals
            .entry(self.name.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Notify::new()))
            .clone();

        warn!(
            "Restarting service {} in {:.1}s...",
            self.name,
            self.current_delay.as_secs_f64()
        );

        tokio::select! {
            _ = tokio::time::sleep(self.current_delay) => {}
            _ = reload_signal.notified() => {
                info!("Supervisor: Service {} received immediate reload during restart delay", self.name);
                self.current_delay = Duration::from_millis(0);
            }
            _ = self.cancellation_token.cancelled() => {
                info!("Service {} received shutdown signal during restart delay", self.name);
                self.resources.status_plane.insert(self.name.clone(), ServiceStatus::Terminated);
                self.resources.status_changed.notify_waiters();
                return false;
            }
        }

        // Apply exponential backoff for next restart
        self.current_delay = self.policy.next_delay(self.current_delay);
        true
    }

    /// Main supervision loop.
    async fn run_loop(mut self) {
        self.spawn_watcher();

        loop {
            if self.cancellation_token.is_cancelled() {
                info!(
                    "Service {} received shutdown signal, exiting gracefully",
                    self.name
                );
                self.resources
                    .status_plane
                    .insert(self.name.clone(), ServiceStatus::Terminated);
                self.resources.status_changed.notify_waiters();
                break;
            }

            let start_status = self.determine_start_status();
            info!(
                "Starting service: {} with status {:?}",
                self.name, start_status
            );
            self.resources
                .status_plane
                .insert(self.name.clone(), start_status);
            self.resources.status_changed.notify_waiters();
            let start_time = std::time::Instant::now();

            let span = tracing::info_span!("service", name = %self.name);

            // Get or create reload signal
            let reload_signal = self
                .resources
                .reload_signals
                .entry(self.name.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Notify::new()))
                .clone();

            // Create reload token for this generation
            let reload_token = CancellationToken::new();
            let identity = ServiceIdentity::new(
                self.name.clone(),
                self.cancellation_token.clone(),
                reload_token.clone(),
            );

            // Run the service with integrated signal handling (no bridge task)
            let run_clone = self.run.clone();
            let token_for_run = self.cancellation_token.clone();
            let resources_clone = self.resources.clone();
            let reload_token_clone = reload_token.clone();

            let result = __run_service_scope(identity, resources_clone, || async move {
                let service_future =
                    std::panic::AssertUnwindSafe(run_clone(token_for_run).instrument(span))
                        .catch_unwind();

                // Integrated signal handling - replaces the bridge_task
                tokio::select! {
                    res = service_future => res,
                    _ = reload_signal.notified() => {
                        reload_token_clone.cancel();
                        info!("Service reload signal received, waiting for service to exit...");
                        // Return Ok to indicate clean reload, not an error
                        Ok(Ok(()))
                    }
                }
            })
            .await;

            // Handle outcome
            let (next_status, should_restart) = self.handle_outcome(result, &reload_token);

            // Check for cancellation after service exits
            if self.cancellation_token.is_cancelled() {
                info!(
                    "Service {} received shutdown signal, not restarting",
                    self.name
                );
                self.resources
                    .status_plane
                    .insert(self.name.clone(), ServiceStatus::Terminated);
                self.resources.status_changed.notify_waiters();
                break;
            }

            if !should_restart {
                info!("Service {} marked as fatal, not restarting", self.name);
                self.resources
                    .status_plane
                    .insert(self.name.clone(), ServiceStatus::Terminated);
                self.resources.status_changed.notify_waiters();
                break;
            }

            info!(
                "Supervisor: Setting next_status for {} to {:?}",
                self.name, next_status
            );
            self.resources
                .status_plane
                .insert(self.name.clone(), next_status);
            self.resources.status_changed.notify_waiters();

            // Reset delay if service ran successfully for long enough
            if start_time.elapsed() >= self.policy.reset_after {
                self.current_delay = self.policy.initial_delay;
            }

            if !self.wait_for_restart().await {
                break;
            }
        }
    }
}

/// Helper for wave-based service management.
struct ServiceWave<'a> {
    services: Vec<&'a ServiceDescription>,
    priority: u8,
}

impl<'a> ServiceWave<'a> {
    /// Groups services by priority into waves.
    fn from_services(services: &'a [ServiceDescription]) -> BTreeMap<u8, ServiceWave<'a>> {
        let mut waves: BTreeMap<u8, Vec<&'a ServiceDescription>> = BTreeMap::new();
        for service in services {
            waves.entry(service.priority).or_default().push(service);
        }
        waves
            .into_iter()
            .map(|(priority, svcs)| {
                (
                    priority,
                    ServiceWave {
                        services: svcs,
                        priority,
                    },
                )
            })
            .collect()
    }

    /// Waits for all services in this wave to become healthy.
    async fn wait_for_healthy(&self, resources: &DaemonResources, timeout: Duration) {
        let start = std::time::Instant::now();
        let mut all_healthy = false;
        while !all_healthy && start.elapsed() < timeout {
            all_healthy = true;
            for service in &self.services {
                let status = resources
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
                    _ = resources.status_changed.notified() => {}
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                }
            }
        }

        if !all_healthy {
            warn!(
                "Wave priority {} did not reach 'Healthy' status within {:?}, proceeding anyway",
                self.priority, timeout
            );
        }
    }
}

/// Spawn a single service with the given restart policy.
pub async fn spawn_service(
    name: String,
    run: ServiceFn,
    watcher: Option<Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>>,
    policy: RestartPolicy,
    running_tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    resources: DaemonResources,
    cancellation_token: CancellationToken,
) {
    let name_clone = name.clone();
    let supervisor = ServiceSupervisor::new(
        name.clone(),
        run,
        watcher,
        policy,
        resources,
        cancellation_token,
    );

    let handle = tokio::spawn(supervisor.run_loop());
    running_tasks.lock().await.insert(name_clone, handle);
}

/// Spawn all registered services using wave-based priorities.
///
/// This starts services in descending order of their `priority` value.
/// Services with high priority (e.g. SYSTEM = 100) start first.
pub async fn spawn_all_services(
    services: &[ServiceDescription],
    restart_policy: RestartPolicy,
    running_tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    resources: DaemonResources,
) {
    info!("Beginning wave-based startup sequence...");

    let waves = ServiceWave::from_services(services);

    // Process waves in descending order of priority
    for (priority, wave) in waves.into_iter().rev() {
        info!(
            "Starting wave priority {} ({} services)...",
            priority,
            wave.services.len()
        );

        for service in &wave.services {
            spawn_service(
                service.name.clone(),
                service.run.clone(),
                service.watcher.clone(),
                restart_policy,
                running_tasks.clone(),
                resources.clone(),
                service.cancellation_token.clone(),
            )
            .await;
        }

        // Wait for services to become healthy using configurable timeout
        wave.wait_for_healthy(&resources, restart_policy.wave_spawn_timeout)
            .await;
    }

    info!("All startup waves initiated.");
}

/// Stop all running services gracefully using wave-based priorities.
///
/// This stops services in ascending order of their `priority` value.
/// Services with the same priority are shut down concurrently.
pub async fn stop_all_services(
    services: &[ServiceDescription],
    running_tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    resources: DaemonResources,
    daemon_token: CancellationToken,
    grace_period: Duration,
) {
    info!("Beginning wave-based graceful shutdown...");

    let waves = ServiceWave::from_services(services);

    // Process waves in ascending order of priority
    for (priority, wave) in waves {
        info!(
            "Shutting down wave priority {} ({} services)...",
            priority,
            wave.services.len()
        );

        // 1. Parallel Signal: Cancel all services in this wave
        for service in &wave.services {
            service.cancellation_token.cancel();
            resources
                .status_plane
                .insert(service.name.clone(), ServiceStatus::ShuttingDown);
            resources.status_changed.notify_waiters();
        }

        // 2. Parallel Wait: Wait for all services in this wave to finish
        let mut join_handles = Vec::new();
        for service in wave.services {
            let name = &service.name;
            let handle_opt = {
                let mut guard = running_tasks.lock().await;
                guard.remove(name)
            };
            if let Some(handle) = handle_opt {
                join_handles.push((name.clone(), handle));
            }
        }

        let resources_for_shutdown = resources.clone();
        let mut shutdown_futures = Vec::new();
        for (name, mut handle) in join_handles {
            let res = resources_for_shutdown.clone();
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
    daemon_token.cancel();
    info!("All shutdown waves completed. ServiceDaemon stopped.");
}
