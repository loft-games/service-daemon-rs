use anyhow::{Error, Result};
use futures::FutureExt;
use futures::future::BoxFuture;
use std::any::Any;
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, error, warn};

use crate::core::context::{__run_service_scope, DaemonResources, ServiceIdentity};
use crate::core::diagnostics::{
    GenerationDiagnosticsHandle, ShutdownBoundaryKind, ShutdownBoundaryOutcomeSnapshot,
    ShutdownBoundaryResultKind, ShutdownResidualActionKind, run_generation_runtime_probe,
};
use crate::core::provider_init::{ProviderRuntimePhase, with_provider_runtime_phase};
use crate::models::{ServiceFn, ServiceId};

pub(super) type ServiceGenerationOutcome = Result<Result<(), Error>, Box<dyn Any + Send>>;
const DEFAULT_ISOLATED_THREAD_JOIN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IsolatedThreadJoinOutcome {
    Joined,
    TimedOut,
    Panicked,
}

impl IsolatedThreadJoinOutcome {
    pub(super) fn diagnostics_outcome(self) -> ShutdownBoundaryOutcomeSnapshot {
        let (result, action, residual) = match self {
            Self::Joined => (
                ShutdownBoundaryResultKind::Completed,
                ShutdownResidualActionKind::None,
                0,
            ),
            Self::TimedOut => (
                ShutdownBoundaryResultKind::TimedOut,
                ShutdownResidualActionKind::RecordedAndDetached,
                1,
            ),
            Self::Panicked => (
                ShutdownBoundaryResultKind::Panicked,
                ShutdownResidualActionKind::None,
                0,
            ),
        };

        ShutdownBoundaryOutcomeSnapshot {
            boundary: ShutdownBoundaryKind::IsolatedRuntimeJoin,
            result,
            action,
            completed: u64::from(matches!(self, Self::Joined)),
            failed: u64::from(matches!(self, Self::Panicked)),
            residual,
        }
    }
}

#[derive(Debug)]
struct IsolatedRuntimeHandle {
    outcome_rx: tokio::sync::oneshot::Receiver<ServiceGenerationOutcome>,
    thread_join: std::thread::JoinHandle<()>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IsolatedStartupFailureKind {
    ThreadSpawn,
    RuntimeBuild,
    BridgeClosed,
    StartupGateCancelled,
}

#[derive(Debug)]
pub(super) struct IsolatedGenerationStartupError {
    service_name: &'static str,
    pub(super) kind: IsolatedStartupFailureKind,
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

pub(super) fn isolated_generation_error(
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

pub(super) struct ServiceGenerationParts {
    pub(super) service_id: ServiceId,
    pub(super) name: &'static str,
    pub(super) generation: u64,
    pub(super) run: ServiceFn,
    pub(super) cancellation_token: CancellationToken,
    pub(super) reload_token: CancellationToken,
    pub(super) resources: Arc<DaemonResources>,
    pub(super) diagnostics: GenerationDiagnosticsHandle,
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
        let phase = if reload_token.is_cancelled() {
            ProviderRuntimePhase::ReloadGenerationResolve
        } else {
            ProviderRuntimePhase::ServiceGenerationResolve
        };
        let identity = ServiceIdentity::new_generation_with_diagnostics(
            service_id,
            name,
            cancellation_token.clone(),
            reload_token,
            diagnostics,
        );

        __run_service_scope(identity, resources, || async move {
            with_provider_runtime_phase(
                phase,
                AssertUnwindSafe(run(cancellation_token).instrument(span)).catch_unwind(),
            )
            .await
        })
        .await
    })
}

pub(super) struct BodyTaskAbortGuard {
    handle: JoinHandle<ServiceGenerationOutcome>,
    abort_on_drop: bool,
}

impl BodyTaskAbortGuard {
    pub(super) fn new(handle: JoinHandle<ServiceGenerationOutcome>) -> Self {
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

pub(super) fn run_body_service_generation(
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

pub(super) async fn bounded_join_isolated_thread(
    thread_join: std::thread::JoinHandle<()>,
    timeout: Duration,
) -> IsolatedThreadJoinOutcome {
    match tokio::time::timeout(
        timeout,
        tokio::task::spawn_blocking(move || thread_join.join()),
    )
    .await
    {
        Ok(Ok(Ok(()))) => IsolatedThreadJoinOutcome::Joined,
        Ok(Ok(Err(_))) | Ok(Err(_)) => IsolatedThreadJoinOutcome::Panicked,
        Err(_) => IsolatedThreadJoinOutcome::TimedOut,
    }
}

pub(super) fn run_isolated_service_generation(
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
        let diagnostics = parts.diagnostics.clone();

        let isolated_handle = match std::thread::Builder::new()
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
            Ok(thread_join) => IsolatedRuntimeHandle {
                outcome_rx: rx,
                thread_join,
            },
            Err(err) => {
                return isolated_generation_error(
                    name,
                    IsolatedStartupFailureKind::ThreadSpawn,
                    format!(
                        "failed to spawn thread '{}': {}",
                        thread_name_for_error, err
                    ),
                );
            }
        };

        let IsolatedRuntimeHandle {
            outcome_rx,
            thread_join,
        } = isolated_handle;

        let outcome = match outcome_rx.await {
            Ok(outcome) => outcome,
            Err(err) => isolated_generation_error(
                name,
                IsolatedStartupFailureKind::BridgeClosed,
                format!("thread exited before reporting outcome: {}", err),
            ),
        };

        let join_outcome =
            bounded_join_isolated_thread(thread_join, DEFAULT_ISOLATED_THREAD_JOIN_TIMEOUT).await;
        diagnostics.record_shutdown_boundary(join_outcome.diagnostics_outcome());

        match join_outcome {
            IsolatedThreadJoinOutcome::Joined => {}
            IsolatedThreadJoinOutcome::TimedOut => warn!(
                service = %name,
                service_id = %service_id,
                generation,
                "Isolated service thread join timed out after outcome bridge completed"
            ),
            IsolatedThreadJoinOutcome::Panicked => warn!(
                service = %name,
                service_id = %service_id,
                generation,
                "Isolated service thread panicked while joining after outcome bridge completed"
            ),
        }

        outcome
    })
}
