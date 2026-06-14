use std::num::NonZeroUsize;

use tokio::runtime::Handle;

use crate::core::adaptive_scheduling::run_adaptive_scheduling_recommendations;
use crate::core::diagnostics::{RuntimeLane, run_lane_runtime_probe};
use crate::models::{ServiceDescription, ServiceScheduling};

use super::ServiceDaemon;

pub(super) const CONTROL_RUNTIME_WORKER_THREADS: usize = 1;
pub(super) const ISOLATED_STARTUP_CONCURRENCY_LIMIT: usize = 4;

pub(super) struct PreparedRuntimes {
    pub(super) control: Option<Handle>,
    pub(super) standard: Handle,
    pub(super) high_priority: Option<Handle>,
}

pub(super) enum RuntimePreparationError {
    Control(std::io::Error),
    HighPriority(std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HighPriorityCapacityPlan {
    entry_count: usize,
    worker_count: Option<NonZeroUsize>,
}

impl HighPriorityCapacityPlan {
    pub(super) fn from_services(services: &[ServiceDescription]) -> Self {
        let entry_count = services
            .iter()
            .filter(|service| service.scheduling() == ServiceScheduling::HighPriority)
            .count();

        Self::from_entry_count(entry_count, read_available_parallelism())
    }

    pub(super) fn from_entry_count(
        entry_count: usize,
        available_parallelism: Option<NonZeroUsize>,
    ) -> Self {
        let worker_count = if entry_count == 0 {
            None
        } else {
            let cap = match available_parallelism {
                Some(parallelism) => parallelism.get(),
                None => 1,
            };
            NonZeroUsize::new(entry_count.min(cap))
        };

        Self {
            entry_count,
            worker_count,
        }
    }

    pub(super) fn entry_count(&self) -> usize {
        self.entry_count
    }

    pub(super) fn worker_count(&self) -> Option<NonZeroUsize> {
        self.worker_count
    }
}

fn read_available_parallelism() -> Option<NonZeroUsize> {
    std::thread::available_parallelism().ok()
}

impl ServiceDaemon {
    pub(super) fn prepare_startup_runtimes(
        &mut self,
    ) -> Result<PreparedRuntimes, RuntimePreparationError> {
        let control = if self.services.is_empty() {
            None
        } else {
            Some(
                self.ensure_control_runtime()
                    .map_err(RuntimePreparationError::Control)?,
            )
        };

        let high_priority = self
            .ensure_high_priority_runtime()
            .map_err(RuntimePreparationError::HighPriority)?;

        let standard = Handle::current();
        if let Some(runtime) = control.as_ref() {
            self.spawn_runtime_probe(runtime, RuntimeLane::Control);
            self.spawn_adaptive_recommendation_loop(runtime);
        }
        self.spawn_runtime_probe(&standard, RuntimeLane::Standard);
        if let Some(runtime) = high_priority.as_ref() {
            self.spawn_runtime_probe(runtime, RuntimeLane::HighPriority);
        }

        Ok(PreparedRuntimes {
            control,
            standard,
            high_priority,
        })
    }

    pub(super) fn ensure_control_runtime(&mut self) -> std::io::Result<Handle> {
        if self.control_runtime.is_none() {
            self.control_runtime = Some(
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .worker_threads(CONTROL_RUNTIME_WORKER_THREADS)
                    .thread_name("svc-control")
                    .build()?,
            );
        }

        match self.control_runtime.as_ref() {
            Some(runtime) => Ok(runtime.handle().clone()),
            None => Err(std::io::Error::other(
                "control runtime missing after successful creation",
            )),
        }
    }

    pub(super) fn ensure_high_priority_runtime(&mut self) -> std::io::Result<Option<Handle>> {
        let Some(worker_count) = self.high_priority_capacity.worker_count() else {
            return Ok(None);
        };

        if self.high_priority_runtime.is_none() {
            tracing::info!(
                high_priority_entries = self.high_priority_capacity.entry_count(),
                high_priority_worker_threads = worker_count.get(),
                "Creating high-priority runtime from static capacity plan"
            );
            self.high_priority_runtime = Some(
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .worker_threads(worker_count.get())
                    .thread_name("svc-high-priority")
                    .build()?,
            );
        }

        Ok(self
            .high_priority_runtime
            .as_ref()
            .map(|runtime| runtime.handle().clone()))
    }

    pub(super) fn spawn_runtime_probe(&mut self, handle: &Handle, lane: RuntimeLane) {
        let diagnostics = self.diagnostics.clone();
        let token = self.cancellation_token.clone();
        self.runtime_probe_tasks
            .push(handle.spawn(run_lane_runtime_probe(diagnostics, lane, token)));
    }

    pub(super) fn spawn_adaptive_recommendation_loop(&mut self, handle: &Handle) {
        if self.adaptive_recommendation_task.is_some()
            || !self.scheduling_advisory_profile.is_enabled()
        {
            return;
        }

        let diagnostics = self.diagnostics.clone();
        let token = self.cancellation_token.clone();
        self.adaptive_recommendation_task =
            Some(handle.spawn(run_adaptive_scheduling_recommendations(diagnostics, token)));
    }

    pub(super) async fn stop_runtime_probes(&mut self) {
        for handle in self.runtime_probe_tasks.drain(..) {
            if let Err(err) = handle.await
                && !err.is_cancelled()
            {
                tracing::warn!(error = ?err, "Runtime probe task ended unexpectedly");
            }
        }
    }

    pub(super) async fn stop_adaptive_recommendation_loop(&mut self) {
        if let Some(handle) = self.adaptive_recommendation_task.take()
            && let Err(err) = handle.await
            && !err.is_cancelled()
        {
            tracing::error!(error = ?err, "Adaptive scheduling recommendation task ended unexpectedly");
        }
    }

    pub(super) fn abort_adaptive_recommendation_loop(&mut self) {
        if let Some(handle) = self.adaptive_recommendation_task.take() {
            handle.abort();
        }
    }

    pub(super) fn shutdown_high_priority_runtime(&mut self) {
        if let Some(runtime) = self.high_priority_runtime.take()
            && let Err(panic) = std::thread::spawn(move || drop(runtime)).join()
        {
            tracing::error!(?panic, "High-priority runtime shutdown thread panicked");
        }
    }

    pub(super) fn shutdown_control_runtime(&mut self) {
        if let Some(runtime) = self.control_runtime.take()
            && let Err(panic) = std::thread::spawn(move || drop(runtime)).join()
        {
            tracing::error!(?panic, "Control runtime shutdown thread panicked");
        }
    }

    pub(super) fn shutdown_high_priority_runtime_detached(&mut self) {
        if let Some(runtime) = self.high_priority_runtime.take() {
            let _ = std::thread::spawn(move || drop(runtime));
        }
    }

    pub(super) fn shutdown_control_runtime_detached(&mut self) {
        if let Some(runtime) = self.control_runtime.take() {
            let _ = std::thread::spawn(move || drop(runtime));
        }
    }
}
