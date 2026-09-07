use super::DaemonInstanceInner;
#[cfg(feature = "high-priority")]
pub(super) use super::high_priority::runtime::HighPriorityCapacityPlan;
#[cfg(feature = "high-priority")]
use crate::core::diagnostics::RuntimeLane;
use tokio::runtime::Handle;

pub(super) const CONTROL_RUNTIME_WORKER_THREADS: usize = 1;
pub(super) const ISOLATED_STARTUP_CONCURRENCY_LIMIT: usize = 4;

pub(super) struct PreparedRuntimes {
    pub(super) control: Option<Handle>,
    pub(super) standard: Handle,
    #[cfg(feature = "high-priority")]
    pub(super) high_priority: Option<Handle>,
}

pub(super) enum RuntimePreparationError {
    Control(std::io::Error),
    #[cfg(feature = "high-priority")]
    HighPriority(std::io::Error),
}

impl DaemonInstanceInner {
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

        #[cfg(feature = "high-priority")]
        let high_priority = self
            .ensure_high_priority_runtime()
            .map_err(RuntimePreparationError::HighPriority)?;

        let standard = Handle::current();
        #[cfg(feature = "high-priority")]
        if let Some(runtime) = control.as_ref() {
            self.spawn_runtime_probe(runtime, RuntimeLane::Control);
            self.spawn_adaptive_recommendation_loop(runtime);
        }
        #[cfg(feature = "high-priority")]
        self.spawn_runtime_probe(&standard, RuntimeLane::Standard);
        #[cfg(feature = "high-priority")]
        if high_priority.is_some() {
            self.spawn_high_priority_runtime_probes();
        }

        Ok(PreparedRuntimes {
            control,
            standard,
            #[cfg(feature = "high-priority")]
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

    pub(super) fn shutdown_control_runtime(&mut self) {
        if let Some(runtime) = self.control_runtime.take()
            && let Err(panic) = std::thread::spawn(move || drop(runtime)).join()
        {
            tracing::error!(?panic, "Control runtime shutdown thread panicked");
        }
    }

    pub(super) fn shutdown_control_runtime_detached(&mut self) {
        if let Some(runtime) = self.control_runtime.take() {
            let _ = std::thread::spawn(move || drop(runtime));
        }
    }
}
