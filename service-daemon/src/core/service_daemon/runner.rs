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
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Notify, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, error, info, warn};

use crate::ProviderInitError;
use crate::ServiceScheduling;
use crate::core::context::{__run_service_scope, DaemonResources, ServiceIdentity};
use crate::core::diagnostics::{
    DiagnosticsStore, GenerationDiagnosticsHandle, GenerationExitKind, RuntimeLane,
    run_generation_runtime_probe,
};
use crate::models::policy::RestartStormGuard;
use crate::models::{
    BackoffController, ServiceDescription, ServiceError, ServiceFn, ServiceId, ServiceStatus,
};

use super::parts::{
    BodyExecutionLane, ServiceSupervisorParts, SpawnAllServicesParts, SpawnServiceParts,
    SupervisorSpawnLane,
};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RestartFailureKind {
    RecoverableError,
    Panic,
    IsolatedStartupFailure,
    InternalSupervisorError,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RestartDecision {
    Immediate,
    WithBackoff(RestartFailureKind),
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IsolatedStartupFailureKind {
    ThreadSpawn,
    RuntimeBuild,
    BridgeClosed,
    StartupGateCancelled,
}

#[derive(Debug)]
struct IsolatedGenerationStartupError {
    service_name: &'static str,
    kind: IsolatedStartupFailureKind,
    message: String,
}

impl fmt::Display for IsolatedGenerationStartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "isolated service generation for '{}' failed during {:?}: {}",
            self.service_name, self.kind, self.message
        )
    }
}

impl std::error::Error for IsolatedGenerationStartupError {}

fn isolated_generation_error(
    name: &'static str,
    kind: IsolatedStartupFailureKind,
    message: String,
) -> ServiceGenerationOutcome {
    Ok(Err(Error::new(IsolatedGenerationStartupError {
        service_name: name,
        kind,
        message,
    })))
}

struct ServiceGenerationParts {
    service_id: ServiceId,
    name: &'static str,
    generation: u64,
    run: ServiceFn,
    cancellation_token: CancellationToken,
    reload_token: CancellationToken,
    resources: Arc<DaemonResources>,
    diagnostics: GenerationDiagnosticsHandle,
}

fn run_scoped_service_generation(
    parts: ServiceGenerationParts,
) -> BoxFuture<'static, ServiceGenerationOutcome> {
    Box::pin(async move {
        let ServiceGenerationParts {
            service_id,
            name,
            generation,
            run,
            cancellation_token,
            reload_token,
            resources,
            diagnostics,
        } = parts;
        let span = tracing::info_span!(
            "service",
            name = %name,
            service_id = %service_id,
            service_id_num = service_id.value(),
            generation,
            runtime_lane = ?diagnostics.runtime_lane(),
        );
        let identity = ServiceIdentity::new_with_diagnostics(
            service_id,
            name,
            cancellation_token.clone(),
            reload_token,
            diagnostics,
        );

        __run_service_scope(identity, resources, || async move {
            AssertUnwindSafe(run(cancellation_token).instrument(span))
                .catch_unwind()
                .await
        })
        .await
    })
}

struct BodyTaskAbortGuard {
    handle: JoinHandle<ServiceGenerationOutcome>,
    abort_on_drop: bool,
}

impl BodyTaskAbortGuard {
    fn new(handle: JoinHandle<ServiceGenerationOutcome>) -> Self {
        Self {
            handle,
            abort_on_drop: true,
        }
    }

    async fn join(mut self) -> Result<ServiceGenerationOutcome, tokio::task::JoinError> {
        let result = (&mut self.handle).await;
        self.abort_on_drop = false;
        result
    }
}

impl Drop for BodyTaskAbortGuard {
    fn drop(&mut self) {
        if self.abort_on_drop {
            self.handle.abort();
        }
    }
}

fn run_body_service_generation(
    parts: ServiceGenerationParts,
    runtime: tokio::runtime::Handle,
) -> BoxFuture<'static, ServiceGenerationOutcome> {
    Box::pin(async move {
        let name = parts.name;
        let service_id = parts.service_id;
        let generation = parts.generation;
        let handle = runtime.spawn(run_scoped_service_generation(parts));
        let guard = BodyTaskAbortGuard::new(handle);

        match guard.join().await {
            Ok(outcome) => outcome,
            Err(err) if err.is_panic() => {
                error!(
                    service = %name,
                    service_id = %service_id,
                    generation,
                    error = ?err,
                    "Service body task panicked outside scoped generation"
                );
                Err(err.into_panic())
            }
            Err(err) => {
                warn!(
                    service = %name,
                    service_id = %service_id,
                    generation,
                    error = ?err,
                    "Service body task ended before reporting outcome"
                );
                Ok(Err(Error::msg(format!(
                    "service '{}' body task ended before reporting outcome: {}",
                    name, err
                ))))
            }
        }
    })
}

fn run_isolated_service_generation(
    parts: ServiceGenerationParts,
    startup_permits: Arc<Semaphore>,
) -> BoxFuture<'static, ServiceGenerationOutcome> {
    Box::pin(async move {
        let name = parts.name;
        let service_id = parts.service_id;
        let generation = parts.generation;
        let cancellation_token = parts.cancellation_token.clone();
        let reload_token = parts.reload_token.clone();
        let permit = tokio::select! {
            permit = startup_permits.acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(err) => {
                    return isolated_generation_error(
                        name,
                        IsolatedStartupFailureKind::StartupGateCancelled,
                        format!("isolated startup gate closed: {}", err),
                    );
                }
            },
            _ = cancellation_token.cancelled() => {
                return isolated_generation_error(
                    name,
                    IsolatedStartupFailureKind::StartupGateCancelled,
                    "shutdown while waiting for isolated startup permit".to_string(),
                );
            }
            _ = reload_token.cancelled() => {
                return isolated_generation_error(
                    name,
                    IsolatedStartupFailureKind::StartupGateCancelled,
                    "reload while waiting for isolated startup permit".to_string(),
                );
            }
        };

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
                    Ok(runtime) => {
                        drop(permit);
                        let probe_diagnostics = parts.diagnostics.clone();
                        runtime.block_on(async move {
                            let probe_token = CancellationToken::new();
                            let probe_handle = tokio::spawn(run_generation_runtime_probe(
                                probe_diagnostics,
                                probe_token.clone(),
                            ));
                            let outcome = run_scoped_service_generation(parts).await;
                            probe_token.cancel();
                            if let Err(err) = probe_handle.await {
                                warn!(
                                    service = %name,
                                    service_id = %service_id,
                                    generation,
                                    error = ?err,
                                    "Isolated runtime probe task ended unexpectedly"
                                );
                            }
                            outcome
                        })
                    }
                    Err(err) => {
                        drop(permit);
                        isolated_generation_error(
                            name,
                            IsolatedStartupFailureKind::RuntimeBuild,
                            format!("failed to create private tokio runtime: {}", err),
                        )
                    }
                };
                let _ = tx.send(outcome);
            }) {
            Ok(_) => match rx.await {
                Ok(outcome) => outcome,
                Err(err) => isolated_generation_error(
                    name,
                    IsolatedStartupFailureKind::BridgeClosed,
                    format!("thread exited before reporting outcome: {}", err),
                ),
            },
            Err(err) => isolated_generation_error(
                name,
                IsolatedStartupFailureKind::ThreadSpawn,
                format!(
                    "failed to spawn thread '{}': {}",
                    thread_name_for_error, err
                ),
            ),
        }
    })
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
    scheduling: ServiceScheduling,
    body_lane: BodyExecutionLane,
    backoff: BackoffController,
    restart_storm: RestartStormGuard,
    resources: Arc<DaemonResources>,
    diagnostics: Arc<DiagnosticsStore>,
    isolated_startup_permits: Arc<Semaphore>,
    cancellation_token: CancellationToken,
    daemon_token: CancellationToken,

    // -- Per-generation mutable context (set during `on_starting`) --
    /// Tracks how long the current generation has been running.
    generation_start: Option<Instant>,
    generation: u64,
    generation_diagnostics: Option<GenerationDiagnosticsHandle>,
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
            scheduling,
            body_lane,
            resources,
            diagnostics,
            isolated_startup_permits,
            cancellation_token,
            daemon_token,
        } = parts;

        Self {
            service_id,
            name,
            run,
            watcher,
            scheduling,
            body_lane,
            backoff: BackoffController::new(policy),
            restart_storm: RestartStormGuard::default(),
            resources,
            diagnostics,
            isolated_startup_permits,
            cancellation_token,
            daemon_token,
            generation_start: None,
            generation: 0,
            generation_diagnostics: None,
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
    /// whether the daemon should shut down, what kind of restart policy to apply,
    /// and the internal diagnostics exit classification.
    fn handle_outcome(
        &self,
        result: ServiceGenerationOutcome,
        reload_token: &CancellationToken,
    ) -> (
        ServiceStatus,
        bool,
        bool,
        RestartDecision,
        GenerationExitKind,
    ) {
        let mut should_restart = true;
        let mut should_shutdown_daemon = false;

        let (next_status, restart_decision, mut exit_kind) = match result {
            Ok(Ok(_)) => {
                warn!("Service {} exited normally", self.name);
                (
                    ServiceStatus::Initializing,
                    RestartDecision::Immediate,
                    GenerationExitKind::NormalExit,
                )
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
                        GenerationExitKind::FatalServiceError,
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
                        GenerationExitKind::ProviderInitError,
                    );
                }
                if let Some(startup_err) = e.downcast_ref::<IsolatedGenerationStartupError>() {
                    error!(
                        service = %self.name,
                        startup_failure_kind = ?startup_err.kind,
                        error = ?e,
                        "Service isolated startup failed"
                    );
                    return (
                        ServiceStatus::Recovering(format!("{:?}", e)),
                        should_restart,
                        should_shutdown_daemon,
                        RestartDecision::WithBackoff(RestartFailureKind::IsolatedStartupFailure),
                        GenerationExitKind::IsolatedStartupFailure,
                    );
                }
                error!("Service {} failed: {:?}", self.name, e);
                (
                    ServiceStatus::Recovering(format!("{:?}", e)),
                    RestartDecision::WithBackoff(RestartFailureKind::RecoverableError),
                    GenerationExitKind::RecoverableError,
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
                    RestartDecision::WithBackoff(RestartFailureKind::Panic),
                    GenerationExitKind::Panic,
                )
            }
        };

        if reload_token.is_cancelled() {
            info!(
                "Supervisor: Service {} exited after reload signal",
                self.name
            );
            exit_kind = GenerationExitKind::Reload;
            return (
                ServiceStatus::Restoring,
                true,
                false,
                RestartDecision::Immediate,
                exit_kind,
            );
        }

        (
            next_status,
            should_restart,
            should_shutdown_daemon,
            restart_decision,
            exit_kind,
        )
    }

    fn record_restart_decision(
        &self,
        decision: RestartDecision,
        policy_delay: Duration,
        effective_delay: Duration,
        rate_limited: bool,
    ) {
        if let Some(diagnostics) = self.generation_diagnostics.as_ref() {
            diagnostics.record_restart(
                matches!(decision, RestartDecision::WithBackoff(_)),
                policy_delay,
                effective_delay,
                rate_limited,
            );
        }
    }

    /// Waits for the restart delay, allowing early exit on reload or cancellation.
    /// Returns `true` if restart should proceed, `false` if shutdown was requested.
    /// Immediate restarts after a clean exit or reload do not advance the backoff counter.
    async fn wait_for_restart(&mut self, decision: RestartDecision) -> bool {
        let RestartDecision::WithBackoff(failure_kind) = decision else {
            self.record_restart_decision(decision, Duration::ZERO, Duration::ZERO, false);
            self.backoff.record_success();
            self.restart_storm.reset();
            return true;
        };

        let reload_signal = self
            .resources
            .reload_signals
            .entry(self.service_id)
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone();

        let storm_decision = self
            .restart_storm
            .record_failure(Instant::now(), self.backoff.current_delay());
        let restart_delay = storm_decision.effective_delay;
        self.record_restart_decision(
            decision,
            storm_decision.policy_delay,
            restart_delay,
            storm_decision.rate_limited,
        );
        warn!(
            service = %self.name,
            service_id = %self.service_id,
            generation = self.generation,
            policy_delay_ms = duration_millis(storm_decision.policy_delay),
            effective_delay_ms = duration_millis(restart_delay),
            rate_limited = storm_decision.rate_limited,
            storm_window_failures = storm_decision.window_failures,
            restart_decision = ?decision,
            restart_failure_kind = ?failure_kind,
            "Restarting service after backoff"
        );

        tokio::select! {
            _ = tokio::time::sleep(restart_delay) => {}
            _ = reload_signal.notified() => {
                info!("Supervisor: Service {} received immediate reload during restart delay", self.name);
                // Immediate reload -- reset backoff so we restart right away
                self.backoff.record_success();
                self.restart_storm.reset();
                return true;
            }
            _ = self.cancellation_token.cancelled() => {
                info!("Service {} received shutdown signal during restart delay", self.name);
                self.resources.status_plane.insert(self.service_id, ServiceStatus::Terminated);
                self.resources.status_changed.notify_waiters();
                return false;
            }
        }

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
        self.generation = self.generation.saturating_add(1);
        let runtime_lane = RuntimeLane::from(self.scheduling);
        self.generation_diagnostics = Some(self.diagnostics.register_generation(
            self.service_id,
            self.name,
            self.generation,
            runtime_lane,
        ));
        info!(
            service = %self.name,
            service_id = %self.service_id,
            generation = self.generation,
            scheduling = ?self.scheduling,
            body_lane = ?self.body_lane,
            runtime_lane = ?runtime_lane,
            status = ?start_status,
            "Starting service generation"
        );
        self.resources
            .status_plane
            .insert(self.service_id, start_status);
        self.resources.status_changed.notify_waiters();

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

        let Some(diagnostics) = self.generation_diagnostics.as_ref().cloned() else {
            return SupervisorState::Outcome(Ok(Err(Error::msg(format!(
                "service '{}' entered Running without diagnostics",
                self.name
            )))));
        };

        info!(
            service = %self.name,
            service_id = %self.service_id,
            generation = self.generation,
            scheduling = ?self.scheduling,
            body_lane = ?self.body_lane,
            runtime_lane = ?diagnostics.runtime_lane(),
            "Service generation running"
        );

        let generation_parts = ServiceGenerationParts {
            service_id: self.service_id,
            name: self.name,
            generation: self.generation,
            run: self.run,
            cancellation_token: self.cancellation_token.clone(),
            reload_token: reload_token.clone(),
            resources: self.resources.clone(),
            diagnostics: diagnostics.clone(),
        };
        let mut generation_future = match &self.body_lane {
            BodyExecutionLane::Standard(runtime) | BodyExecutionLane::HighPriority(runtime) => {
                run_body_service_generation(generation_parts, runtime.clone())
            }
            BodyExecutionLane::Isolated => run_isolated_service_generation(
                generation_parts,
                self.isolated_startup_permits.clone(),
            ),
        };

        let result = tokio::select! {
            res = &mut generation_future => res,
            _ = reload_signal.notified() => {
                diagnostics.record_reload_requested();
                reload_token.cancel();
                info!(
                    service = %self.name,
                    service_id = %self.service_id,
                    generation = self.generation,
                    body_lane = ?self.body_lane,
                    "Service reload signal received, waiting for service generation to exit"
                );
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
        let elapsed_ms = self
            .generation_start
            .map(|start| duration_millis(start.elapsed()));

        // Fast path: If shutdown was requested while the service was running,
        // skip outcome processing entirely -- no error logging, no restart.
        if self.cancellation_token.is_cancelled() {
            let generation_snapshot = self.generation_diagnostics.as_ref().map(|diagnostics| {
                diagnostics.record_exit(GenerationExitKind::Shutdown);
                diagnostics.snapshot()
            });
            let sleep_completed = generation_snapshot
                .as_ref()
                .map_or(0, |snapshot| snapshot.aggregate.service_sleep.completed);
            let sleep_interrupted = generation_snapshot
                .as_ref()
                .map_or(0, |snapshot| snapshot.aggregate.service_sleep.interrupted);
            let sleep_drift_total_ms = generation_snapshot.as_ref().map_or(0, |snapshot| {
                snapshot.aggregate.service_sleep.total_drift_ms
            });
            let sleep_drift_max_ms = generation_snapshot
                .as_ref()
                .map_or(0, |snapshot| snapshot.aggregate.service_sleep.max_drift_ms);
            let runtime_probe_count = generation_snapshot.as_ref().map_or(0, |snapshot| {
                snapshot.aggregate.runtime_probe.completed
                    + snapshot.aggregate.runtime_probe.interrupted
            });
            let runtime_probe_max_drift_ms = generation_snapshot
                .as_ref()
                .map_or(0, |snapshot| snapshot.aggregate.runtime_probe.max_drift_ms);

            info!(
                service = %self.name,
                service_id = %self.service_id,
                generation = self.generation,
                elapsed_ms,
                exit_kind = ?GenerationExitKind::Shutdown,
                sleep_completed,
                sleep_interrupted,
                sleep_drift_total_ms,
                sleep_drift_max_ms,
                runtime_probe_count,
                runtime_probe_max_drift_ms,
                "Service generation exited during shutdown"
            );
            return self.terminate();
        }

        let Some(reload_token) = self.reload_token.as_ref() else {
            let message = format!(
                "service '{}' entered Outcome without a reload token",
                self.name
            );
            error!(
                service = %self.name,
                service_id = %self.service_id,
                generation = self.generation,
                message = %message,
                "Service generation outcome missing reload token"
            );
            self.resources
                .status_plane
                .insert(self.service_id, ServiceStatus::Recovering(message));
            self.resources.status_changed.notify_waiters();
            return SupervisorState::Restart(RestartDecision::WithBackoff(
                RestartFailureKind::InternalSupervisorError,
            ));
        };

        let (next_status, should_restart, should_shutdown_daemon, restart_decision, exit_kind) =
            self.handle_outcome(result, reload_token);

        let generation_snapshot = self.generation_diagnostics.as_ref().map(|diagnostics| {
            diagnostics.record_exit(exit_kind);
            diagnostics.snapshot()
        });
        let sleep_completed = generation_snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.aggregate.service_sleep.completed);
        let sleep_interrupted = generation_snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.aggregate.service_sleep.interrupted);
        let sleep_drift_total_ms = generation_snapshot.as_ref().map_or(0, |snapshot| {
            snapshot.aggregate.service_sleep.total_drift_ms
        });
        let sleep_drift_max_ms = generation_snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.aggregate.service_sleep.max_drift_ms);
        let runtime_probe_count = generation_snapshot.as_ref().map_or(0, |snapshot| {
            snapshot.aggregate.runtime_probe.completed
                + snapshot.aggregate.runtime_probe.interrupted
        });
        let runtime_probe_max_drift_ms = generation_snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.aggregate.runtime_probe.max_drift_ms);

        info!(
            service = %self.name,
            service_id = %self.service_id,
            generation = self.generation,
            next_status = ?next_status,
            should_restart,
            should_shutdown_daemon,
            restart_decision = ?restart_decision,
            elapsed_ms,
            exit_kind = ?exit_kind,
            sleep_completed,
            sleep_interrupted,
            sleep_drift_total_ms,
            sleep_drift_max_ms,
            runtime_probe_count,
            runtime_probe_max_drift_ms,
            "Service generation outcome processed"
        );

        if should_shutdown_daemon {
            self.daemon_token.cancel();
        }

        if !should_restart {
            info!("Service {} marked as fatal, not restarting", self.name);
            return self.terminate();
        }
        self.resources
            .status_plane
            .insert(self.service_id, next_status);
        self.resources.status_changed.notify_waiters();

        if matches!(restart_decision, RestartDecision::WithBackoff(_))
            && let Some(gen_start) = self.generation_start
        {
            let elapsed = gen_start.elapsed();
            self.backoff.maybe_reset(elapsed);
            self.restart_storm
                .maybe_reset(elapsed, self.backoff.policy().reset_after);
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
        if let Some(diagnostics) = self.generation_diagnostics.as_ref() {
            diagnostics.record_terminated();
        }
        info!(
            service = %self.name,
            service_id = %self.service_id,
            generation = self.generation,
            elapsed_ms = self.generation_start.map(|start| duration_millis(start.elapsed())),
            "Service generation terminated"
        );
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
        scheduling,
        supervisor_lane,
        body_lane,
        running_tasks,
        resources,
        diagnostics,
        isolated_startup_permits,
        cancellation_token,
        daemon_token,
    } = parts;

    let supervisor = ServiceSupervisor::new(ServiceSupervisorParts {
        service_id,
        name,
        run,
        watcher,
        policy,
        scheduling,
        body_lane,
        resources,
        diagnostics,
        isolated_startup_permits,
        cancellation_token,
        daemon_token,
    });

    let handle = match supervisor_lane {
        SupervisorSpawnLane::Control(runtime) => runtime.spawn(supervisor.run_loop()),
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
pub async fn spawn_all_services(parts: SpawnAllServicesParts) {
    let SpawnAllServicesParts {
        services,
        restart_policy,
        running_tasks,
        resources,
        diagnostics,
        isolated_startup_permits,
        control_runtime,
        standard_runtime,
        high_priority_runtime,
        daemon_token,
    } = parts;

    info!("Beginning wave-based startup sequence...");

    let waves = ServiceWave::from_services(&services);

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
            let (supervisor_lane, body_lane) = match service.entry.scheduling {
                ServiceScheduling::Standard => (
                    SupervisorSpawnLane::Control(control_runtime.clone()),
                    BodyExecutionLane::Standard(standard_runtime.clone()),
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
                        SupervisorSpawnLane::Control(control_runtime.clone()),
                        BodyExecutionLane::HighPriority(runtime),
                    )
                }
                ServiceScheduling::Isolated => (
                    SupervisorSpawnLane::Control(control_runtime.clone()),
                    BodyExecutionLane::Isolated,
                ),
            };

            spawn_service(SpawnServiceParts {
                service_id: service.id,
                name: service.name(),
                run: service.entry.wrapper,
                watcher: service.entry.watcher,
                policy: restart_policy,
                scheduling: service.entry.scheduling,
                supervisor_lane,
                body_lane,
                running_tasks: running_tasks.clone(),
                resources: resources.clone(),
                diagnostics: diagnostics.clone(),
                isolated_startup_permits: isolated_startup_permits.clone(),
                cancellation_token: service.cancellation_token.clone(),
                daemon_token: daemon_token.clone(),
            })
            .await;
        }

        // Wait for services to become healthy using configurable timeout
        wave.wait_for_healthy(&resources, restart_policy.wave_spawn_timeout, &daemon_token)
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
    use super::super::policy::RestartPolicy;
    use super::*;
    use std::sync::LazyLock;

    static WATCHER_THREAD_NAME: LazyLock<Arc<Mutex<Option<String>>>> =
        LazyLock::new(|| Arc::new(Mutex::new(None)));
    static WATCHER_STARTED: LazyLock<Arc<Notify>> = LazyLock::new(|| Arc::new(Notify::new()));

    fn noop_service(_: CancellationToken) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn cancellable_service(token: CancellationToken) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async move {
            token.cancelled().await;
            Ok(())
        })
    }

    fn control_runtime_watcher() -> BoxFuture<'static, ()> {
        Box::pin(async {
            let thread_name = std::thread::current()
                .name()
                .unwrap_or("unnamed")
                .to_string();
            *WATCHER_THREAD_NAME.lock().await = Some(thread_name);
            WATCHER_STARTED.notify_one();
            futures::future::pending::<()>().await;
        })
    }

    fn fast_policy() -> RestartPolicy {
        RestartPolicy {
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
            multiplier: 1.0,
            jitter_factor: 0.0,
            ..RestartPolicy::for_testing()
        }
    }

    #[tokio::test]
    async fn body_bridge_returns_scoped_generation_outcome() {
        let store = Arc::new(DiagnosticsStore::new());
        let service_id = ServiceId::new(42);
        let parts = ServiceGenerationParts {
            service_id,
            name: "body_bridge",
            generation: 1,
            run: noop_service,
            cancellation_token: CancellationToken::new(),
            reload_token: CancellationToken::new(),
            resources: DaemonResources::new(),
            diagnostics: store.register_generation(
                service_id,
                "body_bridge",
                1,
                RuntimeLane::Standard,
            ),
        };

        let outcome = run_body_service_generation(parts, tokio::runtime::Handle::current()).await;

        assert!(matches!(outcome, Ok(Ok(()))));
    }

    #[tokio::test]
    async fn body_task_abort_guard_aborts_spawned_body_task() {
        struct DropNotifier(Arc<Notify>);

        impl Drop for DropNotifier {
            fn drop(&mut self) {
                self.0.notify_one();
            }
        }

        let started = Arc::new(Notify::new());
        let dropped = Arc::new(Notify::new());
        let handle = tokio::spawn({
            let started = started.clone();
            let dropped = dropped.clone();
            async move {
                let _drop_notifier = DropNotifier(dropped);
                started.notify_one();
                futures::future::pending::<ServiceGenerationOutcome>().await
            }
        });
        let guard = BodyTaskAbortGuard::new(handle);

        started.notified().await;
        let dropped_notified = dropped.notified();
        drop(guard);

        tokio::time::timeout(Duration::from_secs(1), dropped_notified)
            .await
            .expect("body task should be aborted when guard is dropped");
    }

    #[tokio::test]
    async fn spawn_service_runs_watcher_on_control_runtime() {
        *WATCHER_THREAD_NAME.lock().await = None;
        let control_runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(1)
            .thread_name("test-control")
            .build()
            .expect("control runtime should build");
        let service_id = ServiceId::new(77);
        let running_tasks = Arc::new(Mutex::new(HashMap::new()));
        let resources = DaemonResources::new();
        let cancellation_token = CancellationToken::new();
        let daemon_token = CancellationToken::new();

        spawn_service(SpawnServiceParts {
            service_id,
            name: "control_watcher",
            run: cancellable_service,
            watcher: Some(control_runtime_watcher),
            policy: RestartPolicy::for_testing(),
            scheduling: ServiceScheduling::Standard,
            supervisor_lane: SupervisorSpawnLane::Control(control_runtime.handle().clone()),
            body_lane: BodyExecutionLane::Standard(tokio::runtime::Handle::current()),
            running_tasks: running_tasks.clone(),
            resources,
            diagnostics: Arc::new(DiagnosticsStore::new()),
            isolated_startup_permits: Arc::new(Semaphore::new(1)),
            cancellation_token: cancellation_token.clone(),
            daemon_token,
        })
        .await;

        tokio::time::timeout(Duration::from_secs(1), WATCHER_STARTED.notified())
            .await
            .expect("watcher should start on control runtime");
        let thread_name = WATCHER_THREAD_NAME
            .lock()
            .await
            .clone()
            .unwrap_or_else(|| "missing".to_string());

        assert!(
            thread_name.starts_with("test-control"),
            "watcher ran on unexpected thread: {}",
            thread_name
        );

        cancellation_token.cancel();
        let handle = { running_tasks.lock().await.remove(&service_id) };
        if let Some(handle) = handle {
            let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;
        }
        std::thread::spawn(move || drop(control_runtime))
            .join()
            .expect("control runtime drop thread should not panic");
    }

    fn test_supervisor(policy: RestartPolicy) -> ServiceSupervisor {
        ServiceSupervisor::new(ServiceSupervisorParts {
            service_id: ServiceId::new(1),
            name: "test_service",
            run: noop_service,
            watcher: None,
            policy,
            scheduling: ServiceScheduling::Standard,
            body_lane: BodyExecutionLane::Standard(tokio::runtime::Handle::current()),
            resources: DaemonResources::new(),
            diagnostics: Arc::new(DiagnosticsStore::new()),
            isolated_startup_permits: Arc::new(Semaphore::new(1)),
            cancellation_token: CancellationToken::new(),
            daemon_token: CancellationToken::new(),
        })
    }

    #[test]
    fn isolated_startup_errors_use_backoff_recovery() {
        let supervisor = ServiceSupervisor::new(ServiceSupervisorParts {
            service_id: ServiceId::new(1),
            name: "isolated_startup",
            run: noop_service,
            watcher: None,
            policy: RestartPolicy::for_testing(),
            scheduling: ServiceScheduling::Isolated,
            body_lane: BodyExecutionLane::Isolated,
            resources: DaemonResources::new(),
            diagnostics: Arc::new(DiagnosticsStore::new()),
            isolated_startup_permits: Arc::new(Semaphore::new(1)),
            cancellation_token: CancellationToken::new(),
            daemon_token: CancellationToken::new(),
        });
        let reload_token = CancellationToken::new();

        let (status, should_restart, should_shutdown_daemon, restart_decision, exit_kind) =
            supervisor.handle_outcome(
                isolated_generation_error(
                    "isolated_startup",
                    IsolatedStartupFailureKind::ThreadSpawn,
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
        assert!(matches!(
            restart_decision,
            RestartDecision::WithBackoff(RestartFailureKind::IsolatedStartupFailure)
        ));
        assert_eq!(exit_kind, GenerationExitKind::IsolatedStartupFailure);
    }

    #[tokio::test]
    async fn isolated_startup_gate_cancellation_returns_startup_failure() {
        let store = Arc::new(DiagnosticsStore::new());
        let service_id = ServiceId::new(1);
        let cancellation_token = CancellationToken::new();
        cancellation_token.cancel();
        let generation_parts = ServiceGenerationParts {
            service_id,
            name: "isolated_gate",
            generation: 1,
            run: noop_service,
            cancellation_token,
            reload_token: CancellationToken::new(),
            resources: DaemonResources::new(),
            diagnostics: store.register_generation(
                service_id,
                "isolated_gate",
                1,
                RuntimeLane::Isolated,
            ),
        };

        let outcome =
            run_isolated_service_generation(generation_parts, Arc::new(Semaphore::new(0))).await;

        match outcome {
            Ok(Err(err)) => {
                let startup_err = err
                    .downcast_ref::<IsolatedGenerationStartupError>()
                    .expect("expected isolated startup failure");
                assert_eq!(
                    startup_err.kind,
                    IsolatedStartupFailureKind::StartupGateCancelled
                );
            }
            other => panic!("unexpected isolated startup outcome: {:?}", other),
        }
    }

    #[tokio::test]
    async fn recoverable_errors_use_backoff_recovery() {
        let supervisor = test_supervisor(RestartPolicy::for_testing());
        let reload_token = CancellationToken::new();

        let (status, should_restart, should_shutdown_daemon, restart_decision, exit_kind) =
            supervisor.handle_outcome(Ok(Err(Error::msg("transient"))), &reload_token);

        assert!(matches!(status, ServiceStatus::Recovering(_)));
        assert!(should_restart);
        assert!(!should_shutdown_daemon);
        assert!(matches!(
            restart_decision,
            RestartDecision::WithBackoff(RestartFailureKind::RecoverableError)
        ));
        assert_eq!(exit_kind, GenerationExitKind::RecoverableError);
    }

    #[tokio::test]
    async fn panics_use_backoff_recovery() {
        let supervisor = test_supervisor(RestartPolicy::for_testing());
        let reload_token = CancellationToken::new();

        let (status, should_restart, should_shutdown_daemon, restart_decision, exit_kind) =
            supervisor.handle_outcome(Err(Box::new("boom")), &reload_token);

        assert!(matches!(status, ServiceStatus::Recovering(_)));
        assert!(should_restart);
        assert!(!should_shutdown_daemon);
        assert!(matches!(
            restart_decision,
            RestartDecision::WithBackoff(RestartFailureKind::Panic)
        ));
        assert_eq!(exit_kind, GenerationExitKind::Panic);
    }

    #[tokio::test]
    async fn fatal_service_errors_bypass_restart_guard() {
        let supervisor = test_supervisor(RestartPolicy::for_testing());
        let reload_token = CancellationToken::new();

        let (status, should_restart, should_shutdown_daemon, restart_decision, exit_kind) =
            supervisor.handle_outcome(
                Ok(Err(Error::new(ServiceError::Fatal("fatal".to_string())))),
                &reload_token,
            );

        assert!(matches!(status, ServiceStatus::Terminated));
        assert!(!should_restart);
        assert!(!should_shutdown_daemon);
        assert_eq!(restart_decision, RestartDecision::Immediate);
        assert_eq!(exit_kind, GenerationExitKind::FatalServiceError);
    }

    #[tokio::test]
    async fn provider_init_errors_bypass_restart_guard_and_shutdown_daemon() {
        let supervisor = test_supervisor(RestartPolicy::for_testing());
        let reload_token = CancellationToken::new();

        let (status, should_restart, should_shutdown_daemon, restart_decision, exit_kind) =
            supervisor.handle_outcome(
                Ok(Err(Error::new(ProviderInitError::Cancelled {
                    provider: "config".to_string(),
                }))),
                &reload_token,
            );

        assert!(matches!(status, ServiceStatus::Terminated));
        assert!(!should_restart);
        assert!(should_shutdown_daemon);
        assert!(supervisor.daemon_token.is_cancelled());
        assert_eq!(restart_decision, RestartDecision::Immediate);
        assert_eq!(exit_kind, GenerationExitKind::ProviderInitError);
    }

    #[tokio::test]
    async fn wait_for_restart_shutdown_interrupts_storm_guard_delay() {
        let mut supervisor = test_supervisor(fast_policy());

        for _ in 0..5 {
            assert!(
                supervisor
                    .wait_for_restart(RestartDecision::WithBackoff(
                        RestartFailureKind::RecoverableError,
                    ))
                    .await
            );
        }

        let token = supervisor.cancellation_token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            token.cancel();
        });

        let should_restart = supervisor
            .wait_for_restart(RestartDecision::WithBackoff(
                RestartFailureKind::RecoverableError,
            ))
            .await;

        assert!(!should_restart);
        assert_eq!(
            supervisor
                .resources
                .status_plane
                .get(&supervisor.service_id)
                .map(|status| status.value().clone()),
            Some(ServiceStatus::Terminated)
        );
    }

    #[tokio::test]
    async fn wait_for_restart_reload_interrupts_storm_guard_delay_and_resets() {
        let mut supervisor = test_supervisor(fast_policy());
        let reload_signal = Arc::new(Notify::new());
        supervisor
            .resources
            .reload_signals
            .insert(supervisor.service_id, reload_signal.clone());

        for _ in 0..5 {
            assert!(
                supervisor
                    .wait_for_restart(RestartDecision::WithBackoff(
                        RestartFailureKind::RecoverableError,
                    ))
                    .await
            );
        }

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            reload_signal.notify_one();
        });

        let should_restart = supervisor
            .wait_for_restart(RestartDecision::WithBackoff(
                RestartFailureKind::RecoverableError,
            ))
            .await;
        let after_reset = supervisor
            .restart_storm
            .record_failure(Instant::now(), Duration::from_millis(1));

        assert!(should_restart);
        assert_eq!(supervisor.backoff.attempt_count(), 0);
        assert_eq!(after_reset.window_failures, 1);
        assert!(!after_reset.rate_limited);
    }
}
