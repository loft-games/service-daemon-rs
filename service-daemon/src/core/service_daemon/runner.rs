//! Service runner logic for spawning, supervising, and stopping services.
//!
//! The core abstraction is [`ServiceSupervisor`], which manages a single
//! service's lifecycle using an explicit **Finite State Machine (FSM)**.
//! The FSM transitions through the following states:
//!
//! ```text
//!   Starting --> Running --> Outcome --> Restart --> Starting (loop)
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
use tokio::runtime::Handle;
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, error, info, warn};

use crate::ProviderInitError;
use crate::ServiceScheduling;
use crate::core::context::{__run_service_scope, DaemonResources, ServiceIdentity};
use crate::models::{
    BackoffController, ServiceDescription, ServiceError, ServiceFn, ServiceId, ServiceStatus,
};

use super::parts::{
    GenerationExecutionLane, ServiceSupervisorParts, SpawnServiceParts, SupervisorSpawnLane,
};
use super::policy::RestartPolicy;

type ServiceGenerationOutcome = Result<Result<(), Error>, Box<dyn Any + Send>>;

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
    Outcome(ServiceGenerationOutcome),
    /// Wait for the next restart window before looping back to `Starting`.
    Restart(RestartDecision),
    /// Terminal state -- exit the supervision loop.
    Terminated,
}

#[derive(Clone, Copy)]
enum RestartDecision {
    Immediate,
    WithBackoff,
}

impl RestartDecision {
    fn should_record_failure(self) -> bool {
        matches!(self, Self::WithBackoff)
    }
}

fn isolated_generation_error(name: &'static str, message: String) -> ServiceGenerationOutcome {
    Ok(Err(Error::msg(format!(
        "isolated service generation for '{}' failed: {}",
        name, message
    ))))
}

fn run_scoped_service_generation(
    service_id: ServiceId,
    name: &'static str,
    run_fn: ServiceFn,
    cancellation_token: CancellationToken,
    reload_token: CancellationToken,
    resources: Arc<DaemonResources>,
) -> BoxFuture<'static, ServiceGenerationOutcome> {
    Box::pin(async move {
        let span = tracing::info_span!(
            "service",
            name = %name,
            service_id = %service_id,
            service_id_num = service_id.value(),
        );
        let identity =
            ServiceIdentity::new(service_id, name, cancellation_token.clone(), reload_token);

        __run_service_scope(identity, resources, || async move {
            AssertUnwindSafe(run_fn(cancellation_token).instrument(span))
                .catch_unwind()
                .await
        })
        .await
    })
}

fn run_isolated_service_generation(
    service_id: ServiceId,
    name: &'static str,
    run_fn: ServiceFn,
    cancellation_token: CancellationToken,
    reload_token: CancellationToken,
    resources: Arc<DaemonResources>,
) -> BoxFuture<'static, ServiceGenerationOutcome> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let thread_name = format!("svc-{}", name);
    let thread_name_for_error = thread_name.clone();

    match std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            let outcome = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(run_scoped_service_generation(
                    service_id,
                    name,
                    run_fn,
                    cancellation_token,
                    reload_token,
                    resources,
                )),
                Err(err) => isolated_generation_error(
                    name,
                    format!("failed to create private tokio runtime: {}", err),
                ),
            };
            let _ = tx.send(outcome);
        }) {
        Ok(_) => Box::pin(async move {
            match rx.await {
                Ok(outcome) => outcome,
                Err(err) => isolated_generation_error(
                    name,
                    format!("thread exited before reporting outcome: {}", err),
                ),
            }
        }),
        Err(err) => Box::pin(async move {
            isolated_generation_error(
                name,
                format!(
                    "failed to spawn thread '{}': {}",
                    thread_name_for_error, err
                ),
            )
        }),
    }
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
    generation_lane: GenerationExecutionLane,
    backoff: BackoffController,
    resources: Arc<DaemonResources>,
    cancellation_token: CancellationToken,
    daemon_token: CancellationToken,

    // -- Per-generation mutable context (set during `on_starting`) --
    /// Tracks how long the current generation has been running.
    generation_start: Option<Instant>,
    /// Per-generation token used to detect reload vs. normal exit.
    reload_token: Option<CancellationToken>,
}

impl ServiceSupervisor {
    fn new(parts: ServiceSupervisorParts) -> Self {
        let ServiceSupervisorParts {
            service_id,
            name,
            run,
            watcher,
            policy,
            generation_lane,
            resources,
            cancellation_token,
            daemon_token,
        } = parts;

        Self {
            service_id,
            name,
            run,
            watcher,
            generation_lane,
            backoff: BackoffController::new(policy),
            resources,
            cancellation_token,
            daemon_token,
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
    /// Returns the next lifecycle status, whether a restart should happen,
    /// whether the daemon should shut down, and what kind of restart policy to apply.
    fn handle_outcome(
        &self,
        result: ServiceGenerationOutcome,
        reload_token: &CancellationToken,
    ) -> (ServiceStatus, bool, bool, RestartDecision) {
        let mut should_restart = true;
        let mut should_shutdown_daemon = false;

        let (next_status, restart_decision) = match result {
            Ok(Ok(_)) => {
                warn!("Service {} exited normally", self.name);
                (ServiceStatus::Initializing, RestartDecision::Immediate)
            }
            Ok(Err(e)) => {
                if let Some(svc_err) = e.downcast_ref::<ServiceError>()
                    && matches!(svc_err, ServiceError::Fatal(_))
                {
                    error!(
                        "Service {} encountered fatal error: {:?}",
                        self.name, svc_err
                    );
                    should_restart = false;
                    return (
                        ServiceStatus::Terminated,
                        should_restart,
                        false,
                        RestartDecision::Immediate,
                    );
                }
                if let Some(provider_init_err) = e.downcast_ref::<ProviderInitError>() {
                    error!(
                        "Service {} encountered provider init error: {}",
                        self.name, provider_init_err
                    );
                    should_restart = false;
                    should_shutdown_daemon = true;
                    self.daemon_token.cancel();
                    return (
                        ServiceStatus::Terminated,
                        should_restart,
                        should_shutdown_daemon,
                        RestartDecision::Immediate,
                    );
                }
                error!("Service {} failed: {:?}", self.name, e);
                (
                    ServiceStatus::Recovering(format!("{:?}", e)),
                    RestartDecision::WithBackoff,
                )
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
                (
                    ServiceStatus::Recovering(format!("Panic: {}", panic_msg)),
                    RestartDecision::WithBackoff,
                )
            }
        };

        if reload_token.is_cancelled() {
            info!(
                "Supervisor: Service {} exited after reload signal",
                self.name
            );
            return (
                ServiceStatus::Restoring,
                true,
                false,
                RestartDecision::Immediate,
            );
        }

        (
            next_status,
            should_restart,
            should_shutdown_daemon,
            restart_decision,
        )
    }

    /// Waits for the restart delay, allowing early exit on reload or cancellation.
    /// Returns `true` if restart should proceed, `false` if shutdown was requested.
    /// Immediate restarts after a clean exit or reload do not advance the backoff counter.
    async fn wait_for_restart(&mut self, decision: RestartDecision) -> bool {
        if matches!(decision, RestartDecision::Immediate) {
            if decision.should_record_failure() {
                self.backoff.record_failure();
            } else {
                self.backoff.record_success();
            }
            return true;
        }

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
                return true;
            }
            _ = self.cancellation_token.cancelled() => {
                info!("Service {} received shutdown signal during restart delay", self.name);
                self.resources.status_plane.insert(self.service_id, ServiceStatus::Terminated);
                self.resources.status_changed.notify_waiters();
                return false;
            }
        }

        if decision.should_record_failure() {
            self.backoff.record_failure();
        } else {
            self.backoff.record_success();
        }
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
        let reload_signal = self
            .resources
            .reload_signals
            .entry(self.service_id)
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone();

        let Some(reload_token) = self.reload_token.as_ref().cloned() else {
            return SupervisorState::Outcome(Ok(Err(Error::msg(format!(
                "service '{}' entered Running without a reload token",
                self.name
            )))));
        };

        let mut generation_future = match self.generation_lane {
            GenerationExecutionLane::CurrentRuntime => run_scoped_service_generation(
                self.service_id,
                self.name,
                self.run,
                self.cancellation_token.clone(),
                reload_token.clone(),
                self.resources.clone(),
            ),
            GenerationExecutionLane::Isolated => run_isolated_service_generation(
                self.service_id,
                self.name,
                self.run,
                self.cancellation_token.clone(),
                reload_token.clone(),
                self.resources.clone(),
            ),
        };

        let result = tokio::select! {
            res = &mut generation_future => res,
            _ = reload_signal.notified() => {
                reload_token.cancel();
                info!("Service reload signal received, waiting for service to exit...");
                generation_future.await
            }
        };

        SupervisorState::Outcome(result)
    }

    /// **Outcome** -- analyse the service's exit result.
    ///
    /// Decides whether the service should restart (--> `Restart`) or stop
    /// permanently (--> `Terminated`).
    async fn on_outcome(&mut self, result: ServiceGenerationOutcome) -> SupervisorState {
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

        let (next_status, should_restart, should_shutdown_daemon, restart_decision) =
            self.handle_outcome(result, reload_token);

        if should_shutdown_daemon {
            self.daemon_token.cancel();
        }

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

        if matches!(restart_decision, RestartDecision::WithBackoff)
            && let Some(gen_start) = self.generation_start
        {
            self.backoff.maybe_reset(gen_start.elapsed());
        }

        SupervisorState::Restart(restart_decision)
    }

    /// **Restart** -- apply the chosen restart policy before looping back to `Starting`.
    ///
    /// Returns `Terminated` if shutdown is requested during the wait.
    async fn on_restart(&mut self, decision: RestartDecision) -> SupervisorState {
        if self.wait_for_restart(decision).await {
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
                SupervisorState::Restart(decision) => self.on_restart(decision).await,
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
        let start = Instant::now();
        while start.elapsed() < timeout {
            // Early exit if daemon shutdown was requested
            if daemon_token.is_cancelled() {
                info!(
                    "Wave priority {} startup interrupted by shutdown signal, skipping health check",
                    self.priority
                );
                return;
            }

            // Create notification future BEFORE checking the status to avoid lost notifications
            let notification = resources.status_changed.notified();

            let mut all_healthy = true;
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

            if all_healthy {
                return;
            }

            // Wait for any status change, or a short periodic wake-up (defense in depth)
            tokio::select! {
                _ = notification => {}
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

        warn!(
            "Wave priority {} did not reach 'Healthy' status within {:?}, proceeding anyway",
            self.priority, timeout
        );
    }
}

/// Spawn a single service with the given restart policy.
pub async fn spawn_service(parts: SpawnServiceParts) {
    let SpawnServiceParts {
        service_id,
        name,
        run,
        watcher,
        policy,
        supervisor_lane,
        generation_lane,
        running_tasks,
        resources,
        cancellation_token,
        daemon_token,
    } = parts;

    let supervisor = ServiceSupervisor::new(ServiceSupervisorParts {
        service_id,
        name,
        run,
        watcher,
        policy,
        generation_lane,
        resources,
        cancellation_token,
        daemon_token,
    });

    let handle = match supervisor_lane {
        SupervisorSpawnLane::Standard => tokio::spawn(supervisor.run_loop()),
        SupervisorSpawnLane::HighPriority(runtime) => runtime.spawn(supervisor.run_loop()),
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
    high_priority_runtime: Option<Handle>,
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
            let (supervisor_lane, generation_lane) = match service.entry.scheduling {
                ServiceScheduling::Standard => (
                    SupervisorSpawnLane::Standard,
                    GenerationExecutionLane::CurrentRuntime,
                ),
                ServiceScheduling::HighPriority => {
                    let Some(runtime) = high_priority_runtime.clone() else {
                        error!(
                            service = %service.name(),
                            service_id = %service.id,
                            "HighPriority service is missing the shared high-priority runtime"
                        );
                        resources
                            .status_plane
                            .insert(service.id, ServiceStatus::Terminated);
                        resources.status_changed.notify_waiters();
                        daemon_token.cancel();
                        return;
                    };
                    (
                        SupervisorSpawnLane::HighPriority(runtime),
                        GenerationExecutionLane::CurrentRuntime,
                    )
                }
                ServiceScheduling::Isolated => (
                    SupervisorSpawnLane::Standard,
                    GenerationExecutionLane::Isolated,
                ),
            };

            spawn_service(SpawnServiceParts {
                service_id: service.id,
                name: service.name(),
                run: service.entry.wrapper,
                watcher: service.entry.watcher,
                policy: restart_policy,
                supervisor_lane,
                generation_lane,
                running_tasks: running_tasks.clone(),
                resources: resources.clone(),
                cancellation_token: service.cancellation_token.clone(),
                daemon_token: daemon_token.clone(),
            })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_service(_: CancellationToken) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    #[test]
    fn isolated_startup_errors_use_backoff_recovery() {
        let supervisor = ServiceSupervisor::new(ServiceSupervisorParts {
            service_id: ServiceId::new(1),
            name: "isolated_startup",
            run: noop_service,
            watcher: None,
            policy: RestartPolicy::for_testing(),
            generation_lane: GenerationExecutionLane::Isolated,
            resources: DaemonResources::new(),
            cancellation_token: CancellationToken::new(),
            daemon_token: CancellationToken::new(),
        });
        let reload_token = CancellationToken::new();

        let (status, should_restart, should_shutdown_daemon, restart_decision) = supervisor
            .handle_outcome(
                isolated_generation_error(
                    "isolated_startup",
                    "failed to spawn thread 'svc-isolated_startup'".to_string(),
                ),
                &reload_token,
            );

        assert!(matches!(
            status,
            ServiceStatus::Recovering(message) if message.contains("failed to spawn thread")
        ));
        assert!(should_restart);
        assert!(!should_shutdown_daemon);
        assert!(matches!(restart_decision, RestartDecision::WithBackoff));
    }
}
