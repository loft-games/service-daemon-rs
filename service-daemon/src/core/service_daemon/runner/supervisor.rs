use anyhow::Error;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::ServiceScheduling;
use crate::core::context::{__run_daemon_resources_sync_scope, DaemonResources};
use crate::core::diagnostics::{
    DiagnosticsStore, GenerationDiagnosticsHandle, GenerationExitKind,
    RestartDecisionKind as DiagnosticsRestartDecisionKind, RuntimeLane,
};
use crate::core::trigger_runner::{TriggerDispatchFailure, TriggerDispatchFailureKind};
use crate::models::policy::RestartStormGuard;
use crate::models::{BackoffController, ServiceError, ServiceFn, ServiceInstanceId, ServiceStatus};
use crate::{ProviderDependencyWatchSet, ProviderInitError};

use super::super::parts::{
    BodyExecutionLane, BodyExecutionLanes, BodyLaneResolver, ServiceSupervisorParts,
    SpawnServiceParts, SupervisorSpawnLane,
};
use super::generation::{
    IsolatedGenerationStartupError, ServiceGenerationOutcome, ServiceGenerationParts,
    run_body_service_generation, run_isolated_service_generation,
};

/// Represents the discrete states of the service supervision lifecycle.
///
/// Each variant maps to a dedicated handler method on [`ServiceSupervisor`],
/// keeping the control flow flat and each concern isolated.
pub(super) enum SupervisorState {
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
pub(super) enum GenerationResultKind {
    NormalExit,
    RecoverableError,
    Panic,
    FatalServiceError,
    ProviderInitError,
    Reload,
    IsolatedStartupFailure,
}

impl GenerationResultKind {
    pub(super) fn diagnostics_exit_kind(self) -> GenerationExitKind {
        match self {
            Self::NormalExit => GenerationExitKind::NormalExit,
            Self::RecoverableError => GenerationExitKind::RecoverableError,
            Self::Panic => GenerationExitKind::Panic,
            Self::FatalServiceError => GenerationExitKind::FatalServiceError,
            Self::ProviderInitError => GenerationExitKind::ProviderInitError,
            Self::Reload => GenerationExitKind::Reload,
            Self::IsolatedStartupFailure => GenerationExitKind::IsolatedStartupFailure,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct GenerationSignalFacts {
    pub(super) reload_requested: bool,
    pub(super) shutdown_requested: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct GenerationExitRecord {
    pub(super) next_status: ServiceStatus,
    pub(super) should_restart: bool,
    pub(super) should_shutdown_daemon: bool,
    pub(super) restart_decision: RestartDecision,
    pub(super) result: GenerationResultKind,
    pub(super) signals: GenerationSignalFacts,
}

impl GenerationExitRecord {
    pub(super) fn exit_kind(&self) -> GenerationExitKind {
        self.result.diagnostics_exit_kind()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RestartFailureKind {
    RecoverableError,
    Panic,
    IsolatedStartupFailure,
    InternalSupervisorError,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RestartDecision {
    Immediate,
    WithBackoff(RestartFailureKind),
}

impl RestartDecision {
    pub(super) fn diagnostics_kind(self) -> DiagnosticsRestartDecisionKind {
        match self {
            Self::Immediate => DiagnosticsRestartDecisionKind::Immediate,
            Self::WithBackoff(RestartFailureKind::RecoverableError) => {
                DiagnosticsRestartDecisionKind::BackoffRecoverableError
            }
            Self::WithBackoff(RestartFailureKind::Panic) => {
                DiagnosticsRestartDecisionKind::BackoffPanic
            }
            Self::WithBackoff(RestartFailureKind::IsolatedStartupFailure) => {
                DiagnosticsRestartDecisionKind::BackoffIsolatedStartupFailure
            }
            Self::WithBackoff(RestartFailureKind::InternalSupervisorError) => {
                DiagnosticsRestartDecisionKind::BackoffInternalSupervisorError
            }
        }
    }
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}
/// Supervises a single service's lifecycle, including restarts and signal handling.
///
/// Internally driven by a [`SupervisorState`] FSM -- see module-level docs.
pub(super) struct ServiceSupervisor {
    // -- Immutable service identity --
    pub(super) service_instance_id: ServiceInstanceId,
    pub(super) name: &'static str,
    pub(super) run: ServiceFn,
    pub(super) watcher: Option<fn() -> ProviderDependencyWatchSet>,
    pub(super) scheduling: ServiceScheduling,
    pub(super) body_lanes: BodyExecutionLanes,
    pub(super) body_lane_resolver: BodyLaneResolver,
    pub(super) generation_body_lane: Option<BodyExecutionLane>,
    pub(super) generation_scheduling: Option<ServiceScheduling>,
    pub(super) backoff: BackoffController,
    pub(super) restart_storm: RestartStormGuard,
    pub(super) resources: Arc<DaemonResources>,
    pub(super) diagnostics: Arc<DiagnosticsStore>,
    pub(super) isolated_startup_permits: Arc<Semaphore>,
    pub(super) cancellation_token: CancellationToken,
    pub(super) daemon_token: CancellationToken,

    // -- Per-generation mutable context (set during `on_starting`) --
    /// Tracks how long the current generation has been running.
    pub(super) generation_start: Option<Instant>,
    pub(super) generation: u64,
    pub(super) generation_diagnostics: Option<GenerationDiagnosticsHandle>,
    pub(super) dependency_watch_set: Option<ProviderDependencyWatchSet>,
    /// Per-generation token used to detect reload vs. normal exit.
    pub(super) reload_token: Option<CancellationToken>,
}

impl ServiceSupervisor {
    pub(super) fn new(parts: ServiceSupervisorParts) -> Self {
        let ServiceSupervisorParts {
            service_instance_id,
            name,
            run,
            watcher,
            policy,
            scheduling,
            body_lanes,
            body_lane_resolver,
            resources,
            diagnostics,
            isolated_startup_permits,
            cancellation_token,
            daemon_token,
        } = parts;

        Self {
            service_instance_id,
            name,
            run,
            watcher,
            scheduling,
            body_lanes,
            body_lane_resolver,
            generation_body_lane: None,
            generation_scheduling: None,
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
            dependency_watch_set: None,
            reload_token: None,
        }
    }

    /// Determines the initial status for a new service generation.
    fn determine_start_status(&self) -> ServiceStatus {
        let initial_status = self
            .resources
            .status_plane
            .get(&self.service_instance_id)
            .map(|s| s.value().clone())
            .unwrap_or(ServiceStatus::Initializing);

        match initial_status {
            ServiceStatus::Initializing => ServiceStatus::Initializing,
            ServiceStatus::Recovering(e) => ServiceStatus::Recovering(e),
            _ => ServiceStatus::Restoring,
        }
    }

    /// Handles the outcome of a service execution and preserves lifecycle signals
    /// as facts instead of letting reload/shutdown overwrite the generation result.
    pub(super) fn handle_outcome(
        &self,
        result: ServiceGenerationOutcome,
        reload_token: &CancellationToken,
    ) -> GenerationExitRecord {
        let mut should_restart = true;
        let mut should_shutdown_daemon = false;
        let signals = GenerationSignalFacts {
            reload_requested: reload_token.is_cancelled(),
            shutdown_requested: self.cancellation_token.is_cancelled(),
        };

        let (next_status, restart_decision, result) = match result {
            Ok(Ok(_)) => {
                warn!("Service {} exited normally", self.name);
                if signals.shutdown_requested {
                    should_restart = false;
                    return GenerationExitRecord {
                        next_status: ServiceStatus::Terminated,
                        should_restart,
                        should_shutdown_daemon,
                        restart_decision: RestartDecision::Immediate,
                        result: GenerationResultKind::NormalExit,
                        signals,
                    };
                }
                let result = if signals.reload_requested {
                    GenerationResultKind::Reload
                } else {
                    GenerationResultKind::NormalExit
                };
                (
                    if signals.reload_requested {
                        ServiceStatus::Restoring
                    } else {
                        ServiceStatus::Initializing
                    },
                    RestartDecision::Immediate,
                    result,
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
                    return GenerationExitRecord {
                        next_status: ServiceStatus::Terminated,
                        should_restart,
                        should_shutdown_daemon: false,
                        restart_decision: RestartDecision::Immediate,
                        result: GenerationResultKind::FatalServiceError,
                        signals,
                    };
                }
                if let Some(provider_init_err) = e.downcast_ref::<ProviderInitError>() {
                    error!(
                        "Service {} encountered provider init error: {}",
                        self.name, provider_init_err
                    );
                    should_restart = false;
                    should_shutdown_daemon = true;
                    self.daemon_token.cancel();
                    return GenerationExitRecord {
                        next_status: ServiceStatus::Terminated,
                        should_restart,
                        should_shutdown_daemon,
                        restart_decision: RestartDecision::Immediate,
                        result: GenerationResultKind::ProviderInitError,
                        signals,
                    };
                }
                if let Some(startup_err) = e.downcast_ref::<IsolatedGenerationStartupError>() {
                    error!(
                        service = %self.name,
                        startup_failure_kind = ?startup_err.kind,
                        error = ?e,
                        "Service isolated startup failed"
                    );
                    return GenerationExitRecord {
                        next_status: ServiceStatus::Recovering(format!("{:?}", e)),
                        should_restart,
                        should_shutdown_daemon,
                        restart_decision: RestartDecision::WithBackoff(
                            RestartFailureKind::IsolatedStartupFailure,
                        ),
                        result: GenerationResultKind::IsolatedStartupFailure,
                        signals,
                    };
                }
                if let Some(trigger_failure) = e.downcast_ref::<TriggerDispatchFailure>() {
                    let (restart_failure_kind, result) = match trigger_failure.kind() {
                        TriggerDispatchFailureKind::DispatchTaskPanic => {
                            (RestartFailureKind::Panic, GenerationResultKind::Panic)
                        }
                        TriggerDispatchFailureKind::HandlerRetryExhausted
                        | TriggerDispatchFailureKind::DispatchTaskError
                        | TriggerDispatchFailureKind::DispatchTaskCancelled
                        | TriggerDispatchFailureKind::DispatchPermitAcquireFailed
                        | TriggerDispatchFailureKind::DispatchTimedOut
                        | TriggerDispatchFailureKind::ScaleMonitorFailed => (
                            RestartFailureKind::RecoverableError,
                            GenerationResultKind::RecoverableError,
                        ),
                    };
                    error!(
                        service = %self.name,
                        service_instance_id = %self.service_instance_id,
                        generation = self.generation,
                        trigger = %trigger_failure.trigger_name(),
                        trigger_service_id = %trigger_failure.service_instance_id(),
                        instance_seq = ?trigger_failure.instance_seq(),
                        message_id = ?trigger_failure.message_id(),
                        trigger_failure_kind = %trigger_failure.kind().as_str(),
                        exit_kind = ?result.diagnostics_exit_kind(),
                        reload_requested = signals.reload_requested,
                        restart_failure_kind = ?restart_failure_kind,
                        error = ?e,
                        "Trigger dispatch failure ended service generation"
                    );
                    (
                        ServiceStatus::Recovering(format!(
                            "Trigger '{}' dispatch failure ({}): {}",
                            trigger_failure.trigger_name(),
                            trigger_failure.kind(),
                            e
                        )),
                        RestartDecision::WithBackoff(restart_failure_kind),
                        result,
                    )
                } else {
                    error!("Service {} failed: {:?}", self.name, e);
                    (
                        ServiceStatus::Recovering(format!("{:?}", e)),
                        RestartDecision::WithBackoff(RestartFailureKind::RecoverableError),
                        GenerationResultKind::RecoverableError,
                    )
                }
            }
            Err(panic) => {
                let panic_msg = if let Some(s) = panic.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "Unknown panic".to_string()
                };
                error!(
                    reload_requested = signals.reload_requested,
                    "Service {} panicked: {}", self.name, panic_msg
                );
                (
                    ServiceStatus::Recovering(format!("Panic: {}", panic_msg)),
                    RestartDecision::WithBackoff(RestartFailureKind::Panic),
                    GenerationResultKind::Panic,
                )
            }
        };

        GenerationExitRecord {
            next_status,
            should_restart,
            should_shutdown_daemon,
            restart_decision,
            result,
            signals,
        }
    }

    pub(super) fn record_restart_decision(
        &self,
        decision: RestartDecision,
        policy_delay: Duration,
        effective_delay: Duration,
        rate_limited: bool,
    ) {
        if let Some(diagnostics) = self.generation_diagnostics.as_ref() {
            diagnostics.record_restart(
                decision.diagnostics_kind(),
                policy_delay,
                effective_delay,
                rate_limited,
            );
        }
    }

    /// Waits for the restart delay, allowing early exit on reload or cancellation.
    /// Returns `true` if restart should proceed, `false` if shutdown was requested.
    /// Immediate restarts after a clean exit or reload do not advance the backoff counter.
    pub(super) async fn wait_for_restart(&mut self, decision: RestartDecision) -> bool {
        let RestartDecision::WithBackoff(failure_kind) = decision else {
            self.record_restart_decision(decision, Duration::ZERO, Duration::ZERO, false);
            self.resources
                .runtime_facts
                .record_service_restart(self.service_instance_id, None);
            self.backoff.record_success();
            self.restart_storm.reset();
            return true;
        };

        let reload_signal = self
            .resources
            .reload_signals
            .entry(self.service_instance_id)
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone();

        let storm_decision = self
            .restart_storm
            .record_failure(Instant::now(), self.backoff.current_delay());
        let restart_delay = storm_decision.effective_delay;
        self.resources
            .runtime_facts
            .record_service_restart(self.service_instance_id, Some(restart_delay));
        self.record_restart_decision(
            decision,
            storm_decision.policy_delay,
            restart_delay,
            storm_decision.rate_limited,
        );
        warn!(
            service = %self.name,
            service_instance_id = %self.service_instance_id,
            generation = self.generation,
            policy_delay_ms = duration_millis(storm_decision.policy_delay),
            effective_delay_ms = duration_millis(restart_delay),
            rate_limited = storm_decision.rate_limited,
            storm_window_failures = storm_decision.window_failures,
            restart_decision = ?decision,
            restart_decision_kind = ?decision.diagnostics_kind(),
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
                let terminated = ServiceStatus::Terminated;
                self.resources.status_plane.insert(self.service_instance_id, terminated.clone());
                self.resources
                    .runtime_facts
                    .record_service_status(self.service_instance_id, &terminated);
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
    pub(super) async fn on_starting(&mut self) -> SupervisorState {
        if self.cancellation_token.is_cancelled() {
            info!(
                "Service {} received shutdown signal, exiting gracefully",
                self.name
            );
            return self.terminate();
        }

        let start_status = self.determine_start_status();
        self.generation = self.generation.saturating_add(1);
        let resolved_scheduling = self.body_lane_resolver.resolve(
            self.service_instance_id,
            self.generation,
            self.scheduling,
        );
        let runtime_lane = RuntimeLane::from(resolved_scheduling);
        self.generation_diagnostics = Some(self.diagnostics.register_generation(
            self.service_instance_id,
            self.name,
            self.generation,
            runtime_lane,
        ));
        self.generation_scheduling = Some(resolved_scheduling);
        self.generation_body_lane = self.body_lanes.resolve(resolved_scheduling);
        self.generation_start = Some(Instant::now());
        self.reload_token = Some(CancellationToken::new());
        self.dependency_watch_set = match self.watcher {
            Some(watcher) => {
                let watch_set =
                    __run_daemon_resources_sync_scope(self.resources.clone(), watcher).await;
                (!watch_set.is_empty()).then_some(watch_set)
            }
            None => None,
        };

        info!(
            service = %self.name,
            service_instance_id = %self.service_instance_id,
            generation = self.generation,
            declared_scheduling = ?self.scheduling,
            resolved_scheduling = ?resolved_scheduling,
            body_lane = ?self.generation_body_lane,
            runtime_lane = ?runtime_lane,
            status = ?start_status,
            "Starting service generation"
        );
        self.resources.runtime_facts.record_service_started(
            self.service_instance_id,
            self.generation,
            &start_status,
        );
        self.resources
            .status_plane
            .insert(self.service_instance_id, start_status);
        self.resources.status_changed.notify_waiters();

        if self.generation_body_lane.is_none() {
            return SupervisorState::Outcome(Ok(Err(Error::msg(format!(
                "service '{}' resolved to HighPriority without an available high-priority runtime",
                self.name
            )))));
        }

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
            .entry(self.service_instance_id)
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

        let Some(body_lane) = self.generation_body_lane.as_ref().cloned() else {
            return SupervisorState::Outcome(Ok(Err(Error::msg(format!(
                "service '{}' entered Running without a resolved body lane",
                self.name
            )))));
        };
        let resolved_scheduling = self.generation_scheduling.unwrap_or(self.scheduling);

        info!(
            service = %self.name,
            service_instance_id = %self.service_instance_id,
            generation = self.generation,
            declared_scheduling = ?self.scheduling,
            resolved_scheduling = ?resolved_scheduling,
            body_lane = ?body_lane,
            runtime_lane = ?diagnostics.runtime_lane(),
            "Service generation running"
        );

        let generation_parts = ServiceGenerationParts {
            service_instance_id: self.service_instance_id,
            name: self.name,
            generation: self.generation,
            run: self.run,
            cancellation_token: self.cancellation_token.clone(),
            reload_token: reload_token.clone(),
            resources: self.resources.clone(),
            diagnostics: diagnostics.clone(),
        };
        let mut generation_future = match &body_lane {
            BodyExecutionLane::Standard(runtime) | BodyExecutionLane::HighPriority(runtime) => {
                run_body_service_generation(generation_parts, runtime.clone())
            }
            BodyExecutionLane::Isolated => run_isolated_service_generation(
                generation_parts,
                self.isolated_startup_permits.clone(),
            ),
        };

        let result = if let Some(watch_set) = self.dependency_watch_set.take() {
            tokio::select! {
                res = &mut generation_future => res,
                change = watch_set.changed() => {
                    diagnostics.record_reload_requested();
                    reload_token.cancel();
                    info!(
                        service = %self.name,
                        service_instance_id = %self.service_instance_id,
                        generation = self.generation,
                        body_lane = ?body_lane,
                        dependency_type_id = ?change.type_id,
                        dependency_change_reason = ?change.reason,
                        "Provider dependency change detected, waiting for service generation to exit"
                    );
                    generation_future.await
                }
                _ = reload_signal.notified() => {
                    diagnostics.record_reload_requested();
                    reload_token.cancel();
                    info!(
                        service = %self.name,
                        service_instance_id = %self.service_instance_id,
                        generation = self.generation,
                        body_lane = ?body_lane,
                        "Service reload signal received, waiting for service generation to exit"
                    );
                    generation_future.await
                }
            }
        } else {
            tokio::select! {
                res = &mut generation_future => res,
                _ = reload_signal.notified() => {
                    diagnostics.record_reload_requested();
                    reload_token.cancel();
                    info!(
                        service = %self.name,
                        service_instance_id = %self.service_instance_id,
                        generation = self.generation,
                        body_lane = ?body_lane,
                        "Service reload signal received, waiting for service generation to exit"
                    );
                    generation_future.await
                }
            }
        };

        SupervisorState::Outcome(result)
    }

    /// **Outcome** -- analyse the service's exit result.
    ///
    /// Decides whether the service should restart (--> `Restart`) or stop
    /// permanently (--> `Terminated`).
    pub(super) async fn on_outcome(&mut self, result: ServiceGenerationOutcome) -> SupervisorState {
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
            let runtime_lane = self
                .generation_diagnostics
                .as_ref()
                .map(|diagnostics| diagnostics.runtime_lane());

            info!(
                service = %self.name,
                service_instance_id = %self.service_instance_id,
                generation = self.generation,
                runtime_lane = ?runtime_lane,
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
                service_instance_id = %self.service_instance_id,
                generation = self.generation,
                message = %message,
                "Service generation outcome missing reload token"
            );
            let recovering = ServiceStatus::Recovering(message);
            self.resources
                .status_plane
                .insert(self.service_instance_id, recovering.clone());
            self.resources
                .runtime_facts
                .record_service_status(self.service_instance_id, &recovering);
            self.resources.status_changed.notify_waiters();
            return SupervisorState::Restart(RestartDecision::WithBackoff(
                RestartFailureKind::InternalSupervisorError,
            ));
        };

        let exit_record = self.handle_outcome(result, reload_token);
        let exit_kind = exit_record.exit_kind();

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
        let runtime_lane = self
            .generation_diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics.runtime_lane());
        let restart_decision_kind = exit_record.restart_decision.diagnostics_kind();

        info!(
            service = %self.name,
            service_instance_id = %self.service_instance_id,
            generation = self.generation,
            runtime_lane = ?runtime_lane,
            next_status = ?exit_record.next_status,
            should_restart = exit_record.should_restart,
            should_shutdown_daemon = exit_record.should_shutdown_daemon,
            restart_decision = ?exit_record.restart_decision,
            restart_decision_kind = ?restart_decision_kind,
            elapsed_ms,
            exit_kind = ?exit_kind,
            generation_result = ?exit_record.result,
            reload_requested = exit_record.signals.reload_requested,
            shutdown_requested = exit_record.signals.shutdown_requested,
            sleep_completed,
            sleep_interrupted,
            sleep_drift_total_ms,
            sleep_drift_max_ms,
            runtime_probe_count,
            runtime_probe_max_drift_ms,
            "Service generation outcome processed"
        );

        if exit_record.should_shutdown_daemon {
            self.daemon_token.cancel();
        }

        if !exit_record.should_restart {
            self.resources
                .runtime_facts
                .record_service_status(self.service_instance_id, &exit_record.next_status);
            info!("Service {} marked as fatal, not restarting", self.name);
            return self.terminate();
        }
        self.resources
            .status_plane
            .insert(self.service_instance_id, exit_record.next_status.clone());
        self.resources
            .runtime_facts
            .record_service_status(self.service_instance_id, &exit_record.next_status);
        self.resources.status_changed.notify_waiters();

        if matches!(
            exit_record.restart_decision,
            RestartDecision::WithBackoff(_)
        ) && let Some(gen_start) = self.generation_start
        {
            let elapsed = gen_start.elapsed();
            self.backoff.maybe_reset(elapsed);
            self.restart_storm
                .maybe_reset(elapsed, self.backoff.policy().reset_after);
        }

        SupervisorState::Restart(exit_record.restart_decision)
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
            service_instance_id = %self.service_instance_id,
            generation = self.generation,
            elapsed_ms = self.generation_start.map(|start| duration_millis(start.elapsed())),
            "Service generation terminated"
        );
        let terminated = ServiceStatus::Terminated;
        self.resources
            .status_plane
            .insert(self.service_instance_id, terminated.clone());
        self.resources
            .runtime_facts
            .record_service_status(self.service_instance_id, &terminated);
        self.resources.status_changed.notify_waiters();
        SupervisorState::Terminated
    }

    /// Main supervision loop -- a flat FSM driver.
    pub(super) async fn run_loop(mut self) {
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
/// Spawn a single service with the given restart policy.
pub(super) async fn spawn_service(parts: SpawnServiceParts) {
    let SpawnServiceParts {
        service_instance_id,
        name,
        run,
        watcher,
        policy,
        scheduling,
        supervisor_lane,
        body_lanes,
        body_lane_resolver,
        running_tasks,
        resources,
        diagnostics,
        isolated_startup_permits,
        cancellation_token,
        daemon_token,
    } = parts;

    let supervisor = ServiceSupervisor::new(ServiceSupervisorParts {
        service_instance_id,
        name,
        run,
        watcher,
        policy,
        scheduling,
        body_lanes,
        body_lane_resolver,
        resources,
        diagnostics,
        isolated_startup_permits,
        cancellation_token,
        daemon_token,
    });

    let handle = match supervisor_lane {
        SupervisorSpawnLane::Control(runtime) => runtime.spawn(supervisor.run_loop()),
    };

    running_tasks
        .lock()
        .await
        .insert(service_instance_id, handle);
}
#[cfg(test)]
mod tests {
    use super::super::generation::{
        BodyTaskAbortGuard, IsolatedStartupFailureKind, IsolatedThreadJoinOutcome,
        bounded_join_isolated_thread, isolated_generation_error,
    };
    use super::*;
    use crate::core::diagnostics::{
        ShutdownBoundaryKind, ShutdownBoundaryResultKind, ShutdownResidualActionKind,
    };
    use crate::core::service_daemon::policy::RestartPolicy;
    use futures::future::BoxFuture;
    use std::collections::{BTreeMap, HashMap};
    use std::fmt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{LazyLock, Mutex as StdMutex};
    use tokio::sync::Mutex;
    use tokio::task::JoinHandle;
    use tracing::field::{Field, Visit};
    use tracing::{Event, Subscriber};
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::prelude::*;

    static WATCHER_THREAD_NAME: LazyLock<Arc<StdMutex<Option<String>>>> =
        LazyLock::new(|| Arc::new(StdMutex::new(None)));
    static WATCHER_STARTED: LazyLock<Arc<Notify>> = LazyLock::new(|| Arc::new(Notify::new()));
    static NO_LIVE_REMAP_THREADS: LazyLock<Arc<Mutex<Vec<String>>>> =
        LazyLock::new(|| Arc::new(Mutex::new(Vec::new())));
    static NO_LIVE_REMAP_RECORD_AGAIN: AtomicBool = AtomicBool::new(false);
    static STANDARD_TO_ISOLATED_THREADS: LazyLock<Arc<Mutex<Vec<String>>>> =
        LazyLock::new(|| Arc::new(Mutex::new(Vec::new())));
    static ISOLATED_TO_STANDARD_THREADS: LazyLock<Arc<Mutex<Vec<String>>>> =
        LazyLock::new(|| Arc::new(Mutex::new(Vec::new())));

    #[derive(Clone, Default)]
    struct CapturedTraceFields {
        events: Arc<StdMutex<Vec<BTreeMap<String, String>>>>,
    }

    #[derive(Default)]
    struct TraceFieldVisitor {
        fields: BTreeMap<String, String>,
    }

    impl Visit for TraceFieldVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            self.fields
                .insert(field.name().to_string(), format!("{:?}", value));
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }

        fn record_bool(&mut self, field: &Field, value: bool) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }

        fn record_i64(&mut self, field: &Field, value: i64) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }

        fn record_u64(&mut self, field: &Field, value: u64) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }
    }

    impl<S> Layer<S> for CapturedTraceFields
    where
        S: Subscriber,
    {
        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            let mut visitor = TraceFieldVisitor::default();
            event.record(&mut visitor);
            self.events.lock().unwrap().push(visitor.fields);
        }
    }

    fn noop_service(_: CancellationToken) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn cancellable_service(token: CancellationToken) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async move {
            token.cancelled().await;
            Ok(())
        })
    }

    async fn record_current_thread_name(records: Arc<Mutex<Vec<String>>>) {
        let thread_name = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_string();
        records.lock().await.push(thread_name);
    }

    async fn wait_for_thread_records(
        records: Arc<Mutex<Vec<String>>>,
        expected_len: usize,
    ) -> Vec<String> {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let snapshot = records.lock().await.clone();
                if snapshot.len() >= expected_len {
                    return snapshot;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("thread records should reach expected length")
    }

    fn no_live_remap_service(_: CancellationToken) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async {
            record_current_thread_name(NO_LIVE_REMAP_THREADS.clone()).await;
            crate::done();

            while !NO_LIVE_REMAP_RECORD_AGAIN.load(Ordering::SeqCst) && !crate::is_shutdown() {
                crate::sleep(Duration::from_millis(5)).await;
            }

            record_current_thread_name(NO_LIVE_REMAP_THREADS.clone()).await;

            while !crate::is_shutdown() {
                crate::sleep(Duration::from_millis(5)).await;
            }

            Ok(())
        })
    }

    fn standard_to_isolated_remap_service(
        _: CancellationToken,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async {
            record_current_thread_name(STANDARD_TO_ISOLATED_THREADS.clone()).await;
            crate::done();

            while !crate::is_shutdown() {
                crate::sleep(Duration::from_millis(5)).await;
            }

            Ok(())
        })
    }

    fn isolated_to_standard_remap_service(
        _: CancellationToken,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async {
            record_current_thread_name(ISOLATED_TO_STANDARD_THREADS.clone()).await;
            crate::done();

            while !crate::is_shutdown() {
                crate::sleep(Duration::from_millis(5)).await;
            }

            Ok(())
        })
    }

    fn control_runtime_watcher() -> ProviderDependencyWatchSet {
        let thread_name = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_string();
        *WATCHER_THREAD_NAME.lock().unwrap() = Some(thread_name);
        WATCHER_STARTED.notify_one();
        ProviderDependencyWatchSet::new()
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

    fn test_body_lanes() -> BodyExecutionLanes {
        BodyExecutionLanes {
            standard: tokio::runtime::Handle::current(),
            high_priority: None,
        }
    }

    fn test_body_lanes_with_high_priority() -> BodyExecutionLanes {
        let current = tokio::runtime::Handle::current();
        BodyExecutionLanes {
            standard: current.clone(),
            high_priority: Some(current),
        }
    }

    struct RemapSupervisorShared {
        resources: Arc<DaemonResources>,
        diagnostics: Arc<DiagnosticsStore>,
        cancellation_token: CancellationToken,
    }

    fn remap_supervisor(
        service_instance_id: ServiceInstanceId,
        name: &'static str,
        run: ServiceFn,
        declared_scheduling: ServiceScheduling,
        body_lane_resolver: BodyLaneResolver,
        shared: RemapSupervisorShared,
    ) -> ServiceSupervisor {
        ServiceSupervisor::new(ServiceSupervisorParts {
            service_instance_id,
            name,
            run,
            watcher: None,
            policy: fast_policy(),
            scheduling: declared_scheduling,
            body_lanes: test_body_lanes(),
            body_lane_resolver,
            resources: shared.resources,
            diagnostics: shared.diagnostics,
            isolated_startup_permits: Arc::new(Semaphore::new(1)),
            cancellation_token: shared.cancellation_token,
            daemon_token: CancellationToken::new(),
        })
    }

    async fn stop_supervisor(cancellation_token: &CancellationToken, handle: JoinHandle<()>) {
        cancellation_token.cancel();
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("supervisor should stop after cancellation")
            .expect("supervisor task should join cleanly");
    }

    #[tokio::test]
    async fn default_body_lane_resolver_preserves_declared_scheduling_at_generation_start() {
        let declared_lanes = [
            ServiceScheduling::Standard,
            ServiceScheduling::HighPriority,
            ServiceScheduling::Isolated,
        ];

        for (index, declared_scheduling) in declared_lanes.into_iter().enumerate() {
            let service_instance_id =
                ServiceInstanceId::new(uuid::Uuid::from_u128((200 + index) as u128));
            let diagnostics = Arc::new(DiagnosticsStore::new());
            let mut supervisor = ServiceSupervisor::new(ServiceSupervisorParts {
                service_instance_id,
                name: "default_resolver",
                run: noop_service,
                watcher: None,
                policy: fast_policy(),
                scheduling: declared_scheduling,
                body_lanes: test_body_lanes_with_high_priority(),
                body_lane_resolver: BodyLaneResolver::default(),
                resources: DaemonResources::new(),
                diagnostics: diagnostics.clone(),
                isolated_startup_permits: Arc::new(Semaphore::new(1)),
                cancellation_token: CancellationToken::new(),
                daemon_token: CancellationToken::new(),
            });

            let state = supervisor.on_starting().await;

            assert!(matches!(state, SupervisorState::Running));
            assert_eq!(supervisor.generation_scheduling, Some(declared_scheduling));
            match (
                declared_scheduling,
                supervisor.generation_body_lane.as_ref(),
            ) {
                (ServiceScheduling::Standard, Some(BodyExecutionLane::Standard(_))) => {}
                (ServiceScheduling::HighPriority, Some(BodyExecutionLane::HighPriority(_))) => {}
                (ServiceScheduling::Isolated, Some(BodyExecutionLane::Isolated)) => {}
                (_, lane) => panic!("unexpected body lane: {:?}", lane),
            }
            assert_eq!(
                diagnostics
                    .generation_snapshot(service_instance_id, 1)
                    .expect("generation diagnostics should be registered")
                    .runtime_lane,
                RuntimeLane::from(declared_scheduling)
            );
        }
    }

    #[tokio::test]
    async fn resolver_change_does_not_move_running_generation_before_reload() {
        NO_LIVE_REMAP_THREADS.lock().await.clear();
        NO_LIVE_REMAP_RECORD_AGAIN.store(false, Ordering::SeqCst);
        let should_isolate_next_generation = Arc::new(AtomicBool::new(false));
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(210));
        let resources = DaemonResources::new();
        let diagnostics = Arc::new(DiagnosticsStore::new());
        let cancellation_token = CancellationToken::new();
        let supervisor = remap_supervisor(
            service_instance_id,
            "no_live_remap",
            no_live_remap_service,
            ServiceScheduling::Standard,
            BodyLaneResolver::with_override({
                let should_isolate_next_generation = should_isolate_next_generation.clone();
                move |_, _, declared_scheduling| {
                    if should_isolate_next_generation.load(Ordering::SeqCst) {
                        ServiceScheduling::Isolated
                    } else {
                        declared_scheduling
                    }
                }
            }),
            RemapSupervisorShared {
                resources,
                diagnostics: diagnostics.clone(),
                cancellation_token: cancellation_token.clone(),
            },
        );
        let handle = tokio::spawn(supervisor.run_loop());

        let first_records = wait_for_thread_records(NO_LIVE_REMAP_THREADS.clone(), 1).await;
        assert_ne!(first_records[0], "svc-no_live_remap");
        should_isolate_next_generation.store(true, Ordering::SeqCst);
        NO_LIVE_REMAP_RECORD_AGAIN.store(true, Ordering::SeqCst);

        let records = wait_for_thread_records(NO_LIVE_REMAP_THREADS.clone(), 2).await;
        assert_ne!(records[1], "svc-no_live_remap");
        assert!(
            diagnostics
                .generation_snapshot(service_instance_id, 2)
                .is_none()
        );

        stop_supervisor(&cancellation_token, handle).await;
    }

    #[tokio::test]
    async fn standard_to_isolated_remap_applies_only_after_reload_boundary() {
        STANDARD_TO_ISOLATED_THREADS.lock().await.clear();
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(220));
        let resources = DaemonResources::new();
        let reload_signal = resources
            .reload_signals
            .entry(service_instance_id)
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone();
        let diagnostics = Arc::new(DiagnosticsStore::new());
        let cancellation_token = CancellationToken::new();
        let supervisor = remap_supervisor(
            service_instance_id,
            "standard_to_isolated_remap",
            standard_to_isolated_remap_service,
            ServiceScheduling::Standard,
            BodyLaneResolver::with_override(|_, generation, declared_scheduling| {
                if generation >= 2 {
                    ServiceScheduling::Isolated
                } else {
                    declared_scheduling
                }
            }),
            RemapSupervisorShared {
                resources,
                diagnostics: diagnostics.clone(),
                cancellation_token: cancellation_token.clone(),
            },
        );
        let handle = tokio::spawn(supervisor.run_loop());

        let first_records = wait_for_thread_records(STANDARD_TO_ISOLATED_THREADS.clone(), 1).await;
        assert_ne!(first_records[0], "svc-standard_to_isolated_remap");
        assert_eq!(
            diagnostics
                .generation_snapshot(service_instance_id, 1)
                .expect("generation 1 diagnostics should exist")
                .runtime_lane,
            RuntimeLane::Standard
        );

        reload_signal.notify_one();
        let records = wait_for_thread_records(STANDARD_TO_ISOLATED_THREADS.clone(), 2).await;
        assert_eq!(records[1], "svc-standard_to_isolated_remap");
        let generation_1 = diagnostics
            .generation_snapshot(service_instance_id, 1)
            .expect("generation 1 diagnostics should exist after reload");
        let generation_2 = diagnostics
            .generation_snapshot(service_instance_id, 2)
            .expect("generation 2 diagnostics should exist after reload");
        assert_eq!(generation_1.runtime_lane, RuntimeLane::Standard);
        assert_eq!(generation_1.aggregate.lifecycle.reload_requested, 1);
        assert_eq!(generation_1.aggregate.lifecycle.reload_exit, 1);
        assert_eq!(generation_2.runtime_lane, RuntimeLane::Isolated);

        stop_supervisor(&cancellation_token, handle).await;
    }

    #[tokio::test]
    async fn isolated_to_standard_remap_applies_only_after_reload_boundary() {
        ISOLATED_TO_STANDARD_THREADS.lock().await.clear();
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(230));
        let resources = DaemonResources::new();
        let reload_signal = resources
            .reload_signals
            .entry(service_instance_id)
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone();
        let diagnostics = Arc::new(DiagnosticsStore::new());
        let cancellation_token = CancellationToken::new();
        let supervisor = remap_supervisor(
            service_instance_id,
            "isolated_to_standard_remap",
            isolated_to_standard_remap_service,
            ServiceScheduling::Isolated,
            BodyLaneResolver::with_override(|_, generation, declared_scheduling| {
                if generation >= 2 {
                    ServiceScheduling::Standard
                } else {
                    declared_scheduling
                }
            }),
            RemapSupervisorShared {
                resources,
                diagnostics: diagnostics.clone(),
                cancellation_token: cancellation_token.clone(),
            },
        );
        let handle = tokio::spawn(supervisor.run_loop());

        let first_records = wait_for_thread_records(ISOLATED_TO_STANDARD_THREADS.clone(), 1).await;
        assert_eq!(first_records[0], "svc-isolated_to_standard_remap");
        assert_eq!(
            diagnostics
                .generation_snapshot(service_instance_id, 1)
                .expect("generation 1 diagnostics should exist")
                .runtime_lane,
            RuntimeLane::Isolated
        );

        reload_signal.notify_one();
        let records = wait_for_thread_records(ISOLATED_TO_STANDARD_THREADS.clone(), 2).await;
        assert_ne!(records[1], "svc-isolated_to_standard_remap");
        let generation_1 = diagnostics
            .generation_snapshot(service_instance_id, 1)
            .expect("generation 1 diagnostics should exist after reload");
        let generation_2 = diagnostics
            .generation_snapshot(service_instance_id, 2)
            .expect("generation 2 diagnostics should exist after reload");
        assert_eq!(generation_1.runtime_lane, RuntimeLane::Isolated);
        assert_eq!(generation_1.aggregate.lifecycle.reload_requested, 1);
        assert_eq!(generation_1.aggregate.lifecycle.reload_exit, 1);
        assert_eq!(generation_2.runtime_lane, RuntimeLane::Standard);

        stop_supervisor(&cancellation_token, handle).await;
    }

    #[tokio::test]
    async fn body_bridge_returns_scoped_generation_outcome() {
        let store = Arc::new(DiagnosticsStore::new());
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(42));
        let parts = ServiceGenerationParts {
            service_instance_id,
            name: "body_bridge",
            generation: 1,
            run: noop_service,
            cancellation_token: CancellationToken::new(),
            reload_token: CancellationToken::new(),
            resources: DaemonResources::new(),
            diagnostics: store.register_generation(
                service_instance_id,
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
        *WATCHER_THREAD_NAME.lock().unwrap() = None;
        let control_runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(1)
            .thread_name("test-control")
            .build()
            .expect("control runtime should build");
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(77));
        let running_tasks = Arc::new(Mutex::new(HashMap::new()));
        let resources = DaemonResources::new();
        let cancellation_token = CancellationToken::new();
        let daemon_token = CancellationToken::new();

        spawn_service(SpawnServiceParts {
            service_instance_id,
            name: "control_watcher",
            run: cancellable_service,
            watcher: Some(control_runtime_watcher),
            policy: RestartPolicy::for_testing(),
            scheduling: ServiceScheduling::Standard,
            supervisor_lane: SupervisorSpawnLane::Control(control_runtime.handle().clone()),
            body_lanes: test_body_lanes(),
            body_lane_resolver: BodyLaneResolver::default(),
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
            .unwrap()
            .clone()
            .unwrap_or_else(|| "missing".to_string());

        assert!(
            thread_name.starts_with("test-control"),
            "watcher ran on unexpected thread: {}",
            thread_name
        );

        cancellation_token.cancel();
        let handle = { running_tasks.lock().await.remove(&service_instance_id) };
        if let Some(handle) = handle {
            let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;
        }
        std::thread::spawn(move || drop(control_runtime))
            .join()
            .expect("control runtime drop thread should not panic");
    }

    fn test_supervisor(policy: RestartPolicy) -> ServiceSupervisor {
        ServiceSupervisor::new(ServiceSupervisorParts {
            service_instance_id: ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            name: "test_service",
            run: noop_service,
            watcher: None,
            policy,
            scheduling: ServiceScheduling::Standard,
            body_lanes: test_body_lanes(),
            body_lane_resolver: BodyLaneResolver::default(),
            resources: DaemonResources::new(),
            diagnostics: Arc::new(DiagnosticsStore::new()),
            isolated_startup_permits: Arc::new(Semaphore::new(1)),
            cancellation_token: CancellationToken::new(),
            daemon_token: CancellationToken::new(),
        })
    }

    fn register_test_generation(supervisor: &mut ServiceSupervisor, generation: u64) {
        supervisor.generation = generation;
        supervisor.reload_token = Some(CancellationToken::new());
        supervisor.generation_diagnostics = Some(supervisor.diagnostics.register_generation(
            supervisor.service_instance_id,
            supervisor.name,
            generation,
            RuntimeLane::Standard,
        ));
    }

    #[test]
    fn restart_decision_maps_to_diagnostics_kind() {
        assert_eq!(
            RestartDecision::Immediate.diagnostics_kind(),
            DiagnosticsRestartDecisionKind::Immediate
        );
        assert_eq!(
            RestartDecision::WithBackoff(RestartFailureKind::RecoverableError).diagnostics_kind(),
            DiagnosticsRestartDecisionKind::BackoffRecoverableError
        );
        assert_eq!(
            RestartDecision::WithBackoff(RestartFailureKind::Panic).diagnostics_kind(),
            DiagnosticsRestartDecisionKind::BackoffPanic
        );
        assert_eq!(
            RestartDecision::WithBackoff(RestartFailureKind::IsolatedStartupFailure)
                .diagnostics_kind(),
            DiagnosticsRestartDecisionKind::BackoffIsolatedStartupFailure
        );
        assert_eq!(
            RestartDecision::WithBackoff(RestartFailureKind::InternalSupervisorError)
                .diagnostics_kind(),
            DiagnosticsRestartDecisionKind::BackoffInternalSupervisorError
        );
    }

    #[tokio::test]
    async fn supervisor_records_restart_decision_into_generation_diagnostics() {
        let mut supervisor = test_supervisor(RestartPolicy::for_testing());
        let generation = 1;
        register_test_generation(&mut supervisor, generation);

        supervisor.record_restart_decision(
            RestartDecision::WithBackoff(RestartFailureKind::Panic),
            Duration::from_millis(10),
            Duration::from_millis(20),
            true,
        );

        let generation = supervisor
            .diagnostics
            .generation_snapshot(supervisor.service_instance_id, generation)
            .expect("generation diagnostics should be present");
        assert_eq!(generation.aggregate.lifecycle.restart, 1);
        assert_eq!(generation.aggregate.lifecycle.backoff_restart, 1);
        assert_eq!(generation.aggregate.lifecycle.rate_limited_restart, 1);
        assert_eq!(generation.aggregate.lifecycle.last_policy_delay_ms, 10);
        assert_eq!(
            generation
                .aggregate
                .lifecycle
                .last_effective_restart_delay_ms,
            20
        );
        assert_eq!(
            generation.aggregate.lifecycle.last_restart_decision,
            Some(DiagnosticsRestartDecisionKind::BackoffPanic)
        );

        let service = supervisor
            .diagnostics
            .service_snapshot(supervisor.service_instance_id)
            .expect("service diagnostics should be present");
        assert_eq!(
            service.aggregate.lifecycle.last_restart_decision,
            Some(DiagnosticsRestartDecisionKind::BackoffPanic)
        );

        let lane = supervisor.diagnostics.lane_snapshot(RuntimeLane::Standard);
        assert_eq!(
            lane.aggregate.lifecycle.last_restart_decision,
            Some(DiagnosticsRestartDecisionKind::BackoffPanic)
        );
    }

    #[tokio::test]
    async fn wait_for_restart_records_immediate_restart_decision() {
        let mut supervisor = test_supervisor(RestartPolicy::for_testing());
        let generation = 1;
        register_test_generation(&mut supervisor, generation);

        assert!(
            supervisor
                .wait_for_restart(RestartDecision::Immediate)
                .await
        );

        let generation = supervisor
            .diagnostics
            .generation_snapshot(supervisor.service_instance_id, generation)
            .expect("generation diagnostics should be present");
        assert_eq!(generation.aggregate.lifecycle.restart, 1);
        assert_eq!(generation.aggregate.lifecycle.backoff_restart, 0);
        assert_eq!(
            generation.aggregate.lifecycle.last_restart_decision,
            Some(DiagnosticsRestartDecisionKind::Immediate)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn restart_backoff_trace_includes_correlation_fields() {
        let mut supervisor = test_supervisor(fast_policy());
        let generation = 7;
        register_test_generation(&mut supervisor, generation);
        let captured = CapturedTraceFields::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let _guard = tracing::subscriber::set_default(subscriber);
        tracing::callsite::rebuild_interest_cache();

        assert!(
            supervisor
                .wait_for_restart(RestartDecision::WithBackoff(
                    RestartFailureKind::RecoverableError,
                ))
                .await
        );

        let events = captured.events.lock().unwrap();
        let event = events
            .iter()
            .find(|event| event.get("restart_decision_kind").is_some())
            .expect("restart trace event should be captured");
        assert_eq!(event.get("service"), Some(&"test_service".to_string()));
        assert!(event.contains_key("service_instance_id"));
        assert_eq!(event.get("generation"), Some(&generation.to_string()));
        assert_eq!(
            event.get("restart_decision_kind"),
            Some(&"BackoffRecoverableError".to_string())
        );
        assert_eq!(
            event.get("restart_failure_kind"),
            Some(&"RecoverableError".to_string())
        );
        assert!(event.contains_key("policy_delay_ms"));
        assert!(event.contains_key("effective_delay_ms"));
        assert_eq!(event.get("rate_limited"), Some(&"false".to_string()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generation_outcome_trace_includes_correlation_fields() {
        let mut supervisor = test_supervisor(fast_policy());
        let generation = 9;
        register_test_generation(&mut supervisor, generation);
        let captured = CapturedTraceFields::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let _guard = tracing::subscriber::set_default(subscriber);

        let state = supervisor
            .on_outcome(Ok(Err(Error::msg("transient"))))
            .await;

        assert!(matches!(state, SupervisorState::Restart(_)));
        let events = captured.events.lock().unwrap();
        let event = events
            .iter()
            .find(|event| event.get("exit_kind").is_some())
            .unwrap_or_else(|| panic!("outcome trace event should be captured: {:?}", *events));
        assert_eq!(event.get("service"), Some(&"test_service".to_string()));
        assert!(event.contains_key("service_instance_id"));
        assert_eq!(event.get("generation"), Some(&generation.to_string()));
        assert_eq!(
            event.get("runtime_lane"),
            Some(&"Some(Standard)".to_string())
        );
        assert_eq!(
            event.get("exit_kind"),
            Some(&"RecoverableError".to_string())
        );
        assert_eq!(
            event.get("restart_decision_kind"),
            Some(&"BackoffRecoverableError".to_string())
        );
        assert_eq!(event.get("should_restart"), Some(&"true".to_string()));
    }

    #[tokio::test]
    async fn isolated_startup_errors_use_backoff_recovery() {
        let supervisor = ServiceSupervisor::new(ServiceSupervisorParts {
            service_instance_id: ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            name: "isolated_startup",
            run: noop_service,
            watcher: None,
            policy: RestartPolicy::for_testing(),
            scheduling: ServiceScheduling::Isolated,
            body_lanes: test_body_lanes(),
            body_lane_resolver: BodyLaneResolver::default(),
            resources: DaemonResources::new(),
            diagnostics: Arc::new(DiagnosticsStore::new()),
            isolated_startup_permits: Arc::new(Semaphore::new(1)),
            cancellation_token: CancellationToken::new(),
            daemon_token: CancellationToken::new(),
        });
        let reload_token = CancellationToken::new();

        let exit_record = supervisor.handle_outcome(
            isolated_generation_error(
                "isolated_startup",
                IsolatedStartupFailureKind::ThreadSpawn,
                "failed to spawn thread 'svc-isolated_startup'".to_string(),
            ),
            &reload_token,
        );

        assert!(matches!(
            exit_record.next_status,
            ServiceStatus::Recovering(ref message) if message.contains("failed to spawn thread")
        ));
        assert!(exit_record.should_restart);
        assert!(!exit_record.should_shutdown_daemon);
        assert!(matches!(
            exit_record.restart_decision,
            RestartDecision::WithBackoff(RestartFailureKind::IsolatedStartupFailure)
        ));
        assert_eq!(
            exit_record.exit_kind(),
            GenerationExitKind::IsolatedStartupFailure
        );
    }

    #[tokio::test]
    async fn isolated_thread_bounded_join_reports_joined() {
        let thread_join = std::thread::spawn(|| {});

        let outcome = bounded_join_isolated_thread(thread_join, Duration::from_secs(1)).await;

        assert_eq!(outcome, IsolatedThreadJoinOutcome::Joined);
    }

    #[tokio::test]
    async fn isolated_thread_bounded_join_reports_panic() {
        let thread_join = std::thread::spawn(|| panic!("join panic"));

        let outcome = bounded_join_isolated_thread(thread_join, Duration::from_secs(1)).await;

        assert_eq!(outcome, IsolatedThreadJoinOutcome::Panicked);
    }

    #[tokio::test]
    async fn isolated_thread_bounded_join_reports_timeout_residual() {
        let thread_join = std::thread::spawn(|| std::thread::sleep(Duration::from_millis(200)));
        let started = Instant::now();

        let outcome = bounded_join_isolated_thread(thread_join, Duration::from_millis(5)).await;

        assert_eq!(outcome, IsolatedThreadJoinOutcome::TimedOut);
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "bounded join should return promptly and leave the late thread as residual work"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    #[test]
    fn isolated_thread_join_outcome_maps_to_shutdown_boundary_diagnostics() {
        let timeout = IsolatedThreadJoinOutcome::TimedOut.diagnostics_outcome();
        assert_eq!(timeout.boundary, ShutdownBoundaryKind::IsolatedRuntimeJoin);
        assert_eq!(timeout.result, ShutdownBoundaryResultKind::TimedOut);
        assert_eq!(
            timeout.action,
            ShutdownResidualActionKind::RecordedAndDetached
        );
        assert_eq!(timeout.residual, 1);

        let panic = IsolatedThreadJoinOutcome::Panicked.diagnostics_outcome();
        assert_eq!(panic.result, ShutdownBoundaryResultKind::Panicked);
        assert_eq!(panic.failed, 1);
        assert_eq!(panic.residual, 0);
    }

    #[tokio::test]
    async fn isolated_startup_gate_cancellation_returns_startup_failure() {
        let store = Arc::new(DiagnosticsStore::new());
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(1));
        let cancellation_token = CancellationToken::new();
        cancellation_token.cancel();
        let generation_parts = ServiceGenerationParts {
            service_instance_id,
            name: "isolated_gate",
            generation: 1,
            run: noop_service,
            cancellation_token,
            reload_token: CancellationToken::new(),
            resources: DaemonResources::new(),
            diagnostics: store.register_generation(
                service_instance_id,
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

        let exit_record =
            supervisor.handle_outcome(Ok(Err(Error::msg("transient"))), &reload_token);

        assert!(matches!(
            exit_record.next_status,
            ServiceStatus::Recovering(_)
        ));
        assert!(exit_record.should_restart);
        assert!(!exit_record.should_shutdown_daemon);
        assert!(matches!(
            exit_record.restart_decision,
            RestartDecision::WithBackoff(RestartFailureKind::RecoverableError)
        ));
        assert_eq!(
            exit_record.exit_kind(),
            GenerationExitKind::RecoverableError
        );
    }

    #[tokio::test]
    async fn runtime_io_service_errors_use_backoff_recovery() {
        let supervisor = test_supervisor(RestartPolicy::for_testing());
        let reload_token = CancellationToken::new();

        let exit_record = supervisor.handle_outcome(
            Ok(Err(Error::new(ServiceError::runtime_io(
                "clone TCP listener",
                std::io::Error::other("descriptor unavailable"),
            )))),
            &reload_token,
        );

        assert!(matches!(
            exit_record.next_status,
            ServiceStatus::Recovering(_)
        ));
        assert!(exit_record.should_restart);
        assert!(!exit_record.should_shutdown_daemon);
        assert!(matches!(
            exit_record.restart_decision,
            RestartDecision::WithBackoff(RestartFailureKind::RecoverableError)
        ));
        assert_eq!(
            exit_record.exit_kind(),
            GenerationExitKind::RecoverableError
        );
    }

    #[tokio::test]
    async fn trigger_dispatch_errors_use_backoff_recovery() {
        let supervisor = test_supervisor(RestartPolicy::for_testing());
        let reload_token = CancellationToken::new();
        let failure = TriggerDispatchFailure::new(
            TriggerDispatchFailureKind::HandlerRetryExhausted,
            "test_trigger",
            ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            Some(7),
            Some(uuid::Uuid::nil()),
            "retry exhausted",
        );

        let exit_record = supervisor.handle_outcome(Ok(Err(Error::new(failure))), &reload_token);

        assert!(
            matches!(exit_record.next_status, ServiceStatus::Recovering(ref message) if message.contains("test_trigger"))
        );
        assert!(exit_record.should_restart);
        assert!(!exit_record.should_shutdown_daemon);
        assert!(matches!(
            exit_record.restart_decision,
            RestartDecision::WithBackoff(RestartFailureKind::RecoverableError)
        ));
        assert_eq!(
            exit_record.exit_kind(),
            GenerationExitKind::RecoverableError
        );
    }

    #[tokio::test]
    async fn trigger_dispatch_panics_use_panic_restart_classification() {
        let supervisor = test_supervisor(RestartPolicy::for_testing());
        let reload_token = CancellationToken::new();
        let failure = TriggerDispatchFailure::new(
            TriggerDispatchFailureKind::DispatchTaskPanic,
            "panic_trigger",
            ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            Some(8),
            Some(uuid::Uuid::nil()),
            "dispatch panicked",
        );

        let exit_record = supervisor.handle_outcome(Ok(Err(Error::new(failure))), &reload_token);

        assert!(
            matches!(exit_record.next_status, ServiceStatus::Recovering(ref message) if message.contains("panic_trigger"))
        );
        assert!(exit_record.should_restart);
        assert!(!exit_record.should_shutdown_daemon);
        assert!(matches!(
            exit_record.restart_decision,
            RestartDecision::WithBackoff(RestartFailureKind::Panic)
        ));
        assert_eq!(exit_record.exit_kind(), GenerationExitKind::Panic);
    }

    #[tokio::test]
    async fn reload_requested_and_recoverable_error_are_recorded_as_parallel_facts() {
        let supervisor = test_supervisor(RestartPolicy::for_testing());
        let reload_token = CancellationToken::new();
        reload_token.cancel();

        let exit_record = supervisor.handle_outcome(
            Ok(Err(Error::msg("transient during reload"))),
            &reload_token,
        );

        assert!(exit_record.signals.reload_requested);
        assert!(matches!(
            exit_record.next_status,
            ServiceStatus::Recovering(_)
        ));
        assert!(exit_record.should_restart);
        assert!(!exit_record.should_shutdown_daemon);
        assert!(matches!(
            exit_record.restart_decision,
            RestartDecision::WithBackoff(RestartFailureKind::RecoverableError)
        ));
        assert_eq!(exit_record.result, GenerationResultKind::RecoverableError);
        assert_eq!(
            exit_record.exit_kind(),
            GenerationExitKind::RecoverableError
        );
    }

    #[tokio::test]
    async fn reload_requested_and_panic_are_recorded_as_parallel_facts() {
        let supervisor = test_supervisor(RestartPolicy::for_testing());
        let reload_token = CancellationToken::new();
        reload_token.cancel();

        let exit_record =
            supervisor.handle_outcome(Err(Box::new("panic during reload")), &reload_token);

        assert!(exit_record.signals.reload_requested);
        assert!(matches!(
            exit_record.next_status,
            ServiceStatus::Recovering(_)
        ));
        assert!(exit_record.should_restart);
        assert!(!exit_record.should_shutdown_daemon);
        assert!(matches!(
            exit_record.restart_decision,
            RestartDecision::WithBackoff(RestartFailureKind::Panic)
        ));
        assert_eq!(exit_record.result, GenerationResultKind::Panic);
        assert_eq!(exit_record.exit_kind(), GenerationExitKind::Panic);
    }

    #[tokio::test]
    async fn reload_requested_and_trigger_dispatch_failure_are_recorded_as_parallel_facts() {
        let supervisor = test_supervisor(RestartPolicy::for_testing());
        let reload_token = CancellationToken::new();
        reload_token.cancel();
        let failure = TriggerDispatchFailure::new(
            TriggerDispatchFailureKind::DispatchTaskError,
            "reload_trigger",
            ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            Some(9),
            Some(uuid::Uuid::nil()),
            "dispatch failed during reload",
        );

        let exit_record = supervisor.handle_outcome(Ok(Err(Error::new(failure))), &reload_token);

        assert!(exit_record.signals.reload_requested);
        assert!(matches!(
            exit_record.next_status,
            ServiceStatus::Recovering(_)
        ));
        assert!(exit_record.should_restart);
        assert!(!exit_record.should_shutdown_daemon);
        assert!(matches!(
            exit_record.restart_decision,
            RestartDecision::WithBackoff(RestartFailureKind::RecoverableError)
        ));
        assert_eq!(exit_record.result, GenerationResultKind::RecoverableError);
        assert_eq!(
            exit_record.exit_kind(),
            GenerationExitKind::RecoverableError
        );
    }

    #[tokio::test]
    async fn panics_use_backoff_recovery() {
        let supervisor = test_supervisor(RestartPolicy::for_testing());
        let reload_token = CancellationToken::new();

        let exit_record = supervisor.handle_outcome(Err(Box::new("boom")), &reload_token);

        assert!(matches!(
            exit_record.next_status,
            ServiceStatus::Recovering(_)
        ));
        assert!(exit_record.should_restart);
        assert!(!exit_record.should_shutdown_daemon);
        assert!(matches!(
            exit_record.restart_decision,
            RestartDecision::WithBackoff(RestartFailureKind::Panic)
        ));
        assert_eq!(exit_record.exit_kind(), GenerationExitKind::Panic);
    }

    #[tokio::test]
    async fn fatal_service_errors_bypass_restart_guard() {
        let supervisor = test_supervisor(RestartPolicy::for_testing());
        let reload_token = CancellationToken::new();

        let exit_record = supervisor.handle_outcome(
            Ok(Err(Error::new(ServiceError::Fatal("fatal".to_string())))),
            &reload_token,
        );

        assert!(matches!(exit_record.next_status, ServiceStatus::Terminated));
        assert!(!exit_record.should_restart);
        assert!(!exit_record.should_shutdown_daemon);
        assert_eq!(exit_record.restart_decision, RestartDecision::Immediate);
        assert_eq!(
            exit_record.exit_kind(),
            GenerationExitKind::FatalServiceError
        );
    }

    #[tokio::test]
    async fn normal_exit_after_instance_cancellation_does_not_restart() {
        let supervisor = test_supervisor(RestartPolicy::for_testing());
        let reload_token = CancellationToken::new();
        supervisor.cancellation_token.cancel();

        let exit_record = supervisor.handle_outcome(Ok(Ok(())), &reload_token);

        assert_eq!(exit_record.next_status, ServiceStatus::Terminated);
        assert!(!exit_record.should_restart);
        assert!(!exit_record.should_shutdown_daemon);
        assert_eq!(exit_record.restart_decision, RestartDecision::Immediate);
        assert_eq!(exit_record.exit_kind(), GenerationExitKind::NormalExit);
    }

    #[tokio::test]
    async fn provider_init_errors_bypass_restart_guard_and_shutdown_daemon() {
        let supervisor = test_supervisor(RestartPolicy::for_testing());
        let reload_token = CancellationToken::new();

        let exit_record = supervisor.handle_outcome(
            Ok(Err(Error::new(ProviderInitError::Cancelled {
                provider: "config".to_string(),
            }))),
            &reload_token,
        );

        assert!(matches!(exit_record.next_status, ServiceStatus::Terminated));
        assert!(!exit_record.should_restart);
        assert!(exit_record.should_shutdown_daemon);
        assert!(supervisor.daemon_token.is_cancelled());
        assert_eq!(exit_record.restart_decision, RestartDecision::Immediate);
        assert_eq!(
            exit_record.exit_kind(),
            GenerationExitKind::ProviderInitError
        );
    }

    #[tokio::test]
    async fn fatal_service_error_records_exit_without_restart_decision() {
        let mut supervisor = test_supervisor(RestartPolicy::for_testing());
        let generation = 1;
        register_test_generation(&mut supervisor, generation);

        let state = supervisor
            .on_outcome(Ok(Err(Error::new(ServiceError::Fatal(
                "fatal".to_string(),
            )))))
            .await;

        assert!(matches!(state, SupervisorState::Terminated));
        let generation = supervisor
            .diagnostics
            .generation_snapshot(supervisor.service_instance_id, generation)
            .expect("generation diagnostics should be present");
        assert_eq!(
            generation.aggregate.lifecycle.last_exit_kind,
            Some(GenerationExitKind::FatalServiceError)
        );
        assert_eq!(generation.aggregate.lifecycle.last_restart_decision, None);
    }

    #[tokio::test]
    async fn provider_init_error_records_exit_without_restart_decision() {
        let mut supervisor = test_supervisor(RestartPolicy::for_testing());
        let generation = 1;
        register_test_generation(&mut supervisor, generation);

        let state = supervisor
            .on_outcome(Ok(Err(Error::new(ProviderInitError::Cancelled {
                provider: "config".to_string(),
            }))))
            .await;

        assert!(matches!(state, SupervisorState::Terminated));
        assert!(supervisor.daemon_token.is_cancelled());
        let generation = supervisor
            .diagnostics
            .generation_snapshot(supervisor.service_instance_id, generation)
            .expect("generation diagnostics should be present");
        assert_eq!(
            generation.aggregate.lifecycle.last_exit_kind,
            Some(GenerationExitKind::ProviderInitError)
        );
        assert_eq!(generation.aggregate.lifecycle.last_restart_decision, None);
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
                .get(&supervisor.service_instance_id)
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
            .insert(supervisor.service_instance_id, reload_signal.clone());

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
