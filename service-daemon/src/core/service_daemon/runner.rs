//! Service runner logic for spawning, supervising, and stopping services.
//!
//! The core abstraction is [`ServiceSupervisor`], which manages a single
//! service's lifecycle using an explicit **Finite State Machine (FSM)**.
//! The FSM transitions through the following states:
//!
//! ```text
//!   Starting --> Running --> Outcome --> Backoff --> Starting (loop)
//!      |            |           |                        |
//!      +------------+-----------+-- Terminated <---------+
//! ```

use anyhow::{Error, Result};
use futures::FutureExt;
use futures::future::BoxFuture;
use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, error, info, warn};

use crate::ServiceScheduling;
use crate::core::context::{__run_service_scope, DaemonResources, ServiceIdentity};
use crate::models::{
    BackoffController, ServiceDescription, ServiceError, ServiceFn, ServiceId, ServiceStatus,
};

use super::policy::RestartPolicy;

// ---------------------------------------------------------------------------
// Supervisor FSM State
// ---------------------------------------------------------------------------

/// Represents the discrete states of the service supervision lifecycle.
///
/// Each variant maps to a dedicated handler method on [`ServiceSupervisor`],
/// keeping the control flow flat and each concern isolated.
enum SupervisorState {
    /// Prepare resources for a new service generation (status, identity, spans).
    Starting,
    /// The service future is actively executing; monitor for completion or signals.
    Running,
    /// The service has exited; analyse the result and decide whether to restart.
    Outcome(Result<Result<(), Error>, Box<dyn Any + Send>>),
    /// Wait for the backoff delay before looping back to `Starting`.
    Backoff,
    /// Terminal state -- exit the supervision loop.
    Terminated,
}

/// Supervises a single service's lifecycle, including restarts and signal handling.
///
/// Internally driven by a [`SupervisorState`] FSM -- see module-level docs.
struct ServiceSupervisor {
    // -- Immutable service identity --
    service_id: ServiceId,
    name: &'static str,
    run: ServiceFn,
    watcher: Option<fn() -> BoxFuture<'static, ()>>,
    backoff: BackoffController,
    resources: Arc<DaemonResources>,
    cancellation_token: CancellationToken,

    // -- Per-generation mutable context (set during `on_starting`) --
    /// Tracks how long the current generation has been running.
    generation_start: Option<std::time::Instant>,
    /// Per-generation token used to detect reload vs. normal exit.
    reload_token: Option<CancellationToken>,
}

impl ServiceSupervisor {
    fn new(
        service_id: ServiceId,
        name: &'static str,
        run: ServiceFn,
        watcher: Option<fn() -> BoxFuture<'static, ()>>,
        policy: RestartPolicy,
        resources: Arc<DaemonResources>,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self {
            service_id,
            name,
            run,
            watcher,
            backoff: BackoffController::new(policy),
            resources,
            cancellation_token,
            generation_start: None,
            reload_token: None,
        }
    }

    /// Spawns the dependency watcher if present.
    fn spawn_watcher(&self) {
        if let Some(watcher) = &self.watcher {
            let n = self.name;
            let sid = self.service_id;
            let ct = self.cancellation_token.clone();
            let res = self.resources.clone();
            let watcher = *watcher;
            tokio::spawn(async move {
                while !ct.is_cancelled() {
                    let reload_signal = res
                        .reload_signals
                        .entry(sid)
                        .or_insert_with(|| Arc::new(Notify::new()))
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
            .get(&self.service_id)
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
            .entry(self.service_id)
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone();

        warn!(
            "Restarting service {} in {:.1}s...",
            self.name,
            self.backoff.current_delay().as_secs_f64()
        );

        tokio::select! {
            _ = tokio::time::sleep(self.backoff.current_delay()) => {}
            _ = reload_signal.notified() => {
                info!("Supervisor: Service {} received immediate reload during restart delay", self.name);
                // Immediate reload -- reset backoff so we restart right away
                self.backoff.record_success();
            }
            _ = self.cancellation_token.cancelled() => {
                info!("Service {} received shutdown signal during restart delay", self.name);
                self.resources.status_plane.insert(self.service_id, ServiceStatus::Terminated);
                self.resources.status_changed.notify_waiters();
                return false;
            }
        }

        // Advance backoff for the next potential restart
        self.backoff.record_failure();
        true
    }

    // -----------------------------------------------------------------------
    // FSM State Handlers
    // -----------------------------------------------------------------------

    /// **Starting** -- prepare resources for a new service generation.
    ///
    /// If shutdown was already requested, transition directly to `Terminated`.
    async fn on_starting(&mut self) -> SupervisorState {
        if self.cancellation_token.is_cancelled() {
            info!(
                "Service {} received shutdown signal, exiting gracefully",
                self.name
            );
            return self.terminate();
        }

        let start_status = self.determine_start_status();
        info!(
            "Starting service: {} with status {:?}",
            self.name, start_status
        );
        self.resources
            .status_plane
            .insert(self.service_id, start_status);
        self.resources.status_changed.notify_waiters();

        // Record generation context for downstream state handlers
        self.generation_start = Some(Instant::now());
        self.reload_token = Some(CancellationToken::new());

        SupervisorState::Running
    }

    /// **Running** -- execute the service future with integrated signal handling.
    ///
    /// Owns the `tokio::select!` that races the service against reload signals,
    /// then hands the raw result off to `Outcome`.
    async fn on_running(&mut self) -> SupervisorState {
        let span = tracing::info_span!(
            "service",
            name = %self.name,
            service_id = %self.service_id,
            service_id_num = self.service_id.value(),
        );

        // Get or create reload signal
        let reload_signal = self
            .resources
            .reload_signals
            .entry(self.service_id)
            .or_insert_with(|| Arc::new(tokio::sync::Notify::new()))
            .clone();

        let reload_token = self
            .reload_token
            .as_ref()
            .expect("reload_token must be set by on_starting")
            .clone();

        let identity = ServiceIdentity::new(
            self.service_id,
            self.name,
            self.cancellation_token.clone(),
            reload_token.clone(),
        );

        let run_fn = self.run;
        let token_for_run = self.cancellation_token.clone();
        let resources_clone = self.resources.clone();
        let reload_token_clone = reload_token;

        let result = __run_service_scope(identity, resources_clone, || async move {
            let service_future =
                AssertUnwindSafe(run_fn(token_for_run).instrument(span)).catch_unwind();

            // Integrated signal handling -- replaces the bridge_task
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

        SupervisorState::Outcome(result)
    }

    /// **Outcome** -- analyse the service's exit result.
    ///
    /// Decides whether the service should restart (--> `Backoff`) or stop
    /// permanently (--> `Terminated`).
    async fn on_outcome(
        &mut self,
        result: Result<Result<(), Error>, Box<dyn Any + Send>>,
    ) -> SupervisorState {
        // Fast path: If shutdown was requested while the service was running,
        // skip outcome processing entirely -- no error logging, no restart.
        if self.cancellation_token.is_cancelled() {
            info!(
                "Service {} exited during shutdown, marking as Terminated",
                self.name
            );
            return self.terminate();
        }

        let reload_token = self
            .reload_token
            .as_ref()
            .expect("reload_token must be set by on_starting");

        let (next_status, should_restart) = self.handle_outcome(result, reload_token);

        if !should_restart {
            info!("Service {} marked as fatal, not restarting", self.name);
            return self.terminate();
        }

        info!(
            "Supervisor: Setting next_status for {} to {:?}",
            self.name, next_status
        );
        self.resources
            .status_plane
            .insert(self.service_id, next_status);
        self.resources.status_changed.notify_waiters();

        // Reset backoff if service ran successfully for long enough
        if let Some(gen_start) = self.generation_start {
            self.backoff.maybe_reset(gen_start.elapsed());
        }

        SupervisorState::Backoff
    }

    /// **Backoff** -- wait for the restart delay before looping back to `Starting`.
    ///
    /// Returns `Terminated` if shutdown is requested during the wait.
    async fn on_backoff(&mut self) -> SupervisorState {
        if self.wait_for_restart().await {
            SupervisorState::Starting
        } else {
            SupervisorState::Terminated
        }
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Mark the service as `Terminated` and notify status listeners.
    fn terminate(&self) -> SupervisorState {
        self.resources
            .status_plane
            .insert(self.service_id, ServiceStatus::Terminated);
        self.resources.status_changed.notify_waiters();
        SupervisorState::Terminated
    }

    /// Main supervision loop -- a flat FSM driver.
    async fn run_loop(mut self) {
        self.spawn_watcher();

        let mut state = SupervisorState::Starting;
        loop {
            state = match state {
                SupervisorState::Starting => self.on_starting().await,
                SupervisorState::Running => self.on_running().await,
                SupervisorState::Outcome(result) => self.on_outcome(result).await,
                SupervisorState::Backoff => self.on_backoff().await,
                SupervisorState::Terminated => break,
            };
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
            waves.entry(service.priority()).or_default().push(service);
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
    ///
    /// Returns early if the `daemon_token` is cancelled, allowing the daemon
    /// to skip waiting during shutdown.
    async fn wait_for_healthy(
        &self,
        resources: &Arc<DaemonResources>,
        timeout: Duration,
        daemon_token: &CancellationToken,
    ) {
        let start = std::time::Instant::now();
        let mut all_healthy = false;
        while !all_healthy && start.elapsed() < timeout {
            // Early exit if daemon shutdown was requested
            if daemon_token.is_cancelled() {
                info!(
                    "Wave priority {} startup interrupted by shutdown signal, skipping health check",
                    self.priority
                );
                return;
            }

            all_healthy = true;
            for service in &self.services {
                let status = resources
                    .status_plane
                    .get(&service.id)
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
                    _ = daemon_token.cancelled() => {
                        info!(
                            "Wave priority {} startup interrupted by shutdown signal",
                            self.priority
                        );
                        return;
                    }
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
#[allow(clippy::too_many_arguments)]
pub async fn spawn_service(
    service_id: ServiceId,
    name: &'static str,
    run: ServiceFn,
    watcher: Option<fn() -> BoxFuture<'static, ()>>,
    policy: RestartPolicy,
    scheduling: ServiceScheduling,
    running_tasks: Arc<Mutex<HashMap<ServiceId, JoinHandle<()>>>>,
    resources: Arc<DaemonResources>,
    cancellation_token: CancellationToken,
) {
    let supervisor = ServiceSupervisor::new(
        service_id,
        name,
        run,
        watcher,
        policy,
        resources,
        cancellation_token,
    );

    let handle = match scheduling {
        ServiceScheduling::Isolated => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let thread_name = format!("svc-{}", name);

            // Spawn a dedicated OS thread for this service
            std::thread::Builder::new()
                .name(thread_name)
                .spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("Failed to create isolated tokio runtime");

                    rt.block_on(supervisor.run_loop());
                    let _ = tx.send(());
                })
                .expect("Failed to spawn isolated service thread");

            // Wrap the thread completion in a tokio task so we can return a JoinHandle
            tokio::spawn(async move {
                let _ = rx.await;
            })
        }
        _ => tokio::spawn(supervisor.run_loop()),
    };

    running_tasks.lock().await.insert(service_id, handle);
}

/// Spawn all registered services using wave-based priorities.
///
/// This starts services in descending order of their `priority` value.
/// Services with high priority (e.g. SYSTEM = 100) start first.
///
/// The `daemon_token` is threaded through to `wait_for_healthy` so that
/// the wave startup sequence can be interrupted immediately if the daemon
/// receives a shutdown signal during startup.
pub async fn spawn_all_services(
    services: &[ServiceDescription],
    restart_policy: RestartPolicy,
    running_tasks: Arc<Mutex<HashMap<ServiceId, JoinHandle<()>>>>,
    resources: Arc<DaemonResources>,
    daemon_token: &CancellationToken,
) {
    info!("Beginning wave-based startup sequence...");

    let waves = ServiceWave::from_services(services);

    // Process waves in descending order of priority
    for (priority, wave) in waves.into_iter().rev() {
        // Skip remaining waves if shutdown was requested
        if daemon_token.is_cancelled() {
            info!("Startup sequence interrupted by shutdown signal, skipping remaining waves");
            break;
        }

        info!(
            "Starting wave priority {} ({} services)...",
            priority,
            wave.services.len()
        );

        for service in &wave.services {
            spawn_service(
                service.id,
                service.name(),
                service.entry.wrapper,
                service.entry.watcher,
                restart_policy,
                service.entry.scheduling,
                running_tasks.clone(),
                resources.clone(),
                service.cancellation_token.clone(),
            )
            .await;
        }

        // Wait for services to become healthy using configurable timeout
        wave.wait_for_healthy(&resources, restart_policy.wave_spawn_timeout, daemon_token)
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
    running_tasks: Arc<Mutex<HashMap<ServiceId, JoinHandle<()>>>>,
    resources: Arc<DaemonResources>,
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
                .insert(service.id, ServiceStatus::ShuttingDown);
            resources.status_changed.notify_waiters();
        }

        // 2. Parallel Wait: Wait for all services in this wave to finish
        let mut join_handles = Vec::new();
        for service in wave.services {
            let sid = service.id;
            let name = service.name();
            let handle_opt = {
                let mut guard = running_tasks.lock().await;
                guard.remove(&sid)
            };
            if let Some(handle) = handle_opt {
                join_handles.push((sid, name, handle));
            }
        }

        let resources_for_shutdown = resources.clone();
        let mut shutdown_futures = Vec::new();
        for (sid, name, mut handle) in join_handles {
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
                res.status_plane.insert(sid, ServiceStatus::Terminated);
                res.status_changed.notify_waiters();
            });
        }
        futures::future::join_all(shutdown_futures).await;
    }

    // Finally, cancel the daemon's own token to signal completion if anyone is watching it
    daemon_token.cancel();
    info!("All shutdown waves completed. ServiceDaemon stopped.");
}
