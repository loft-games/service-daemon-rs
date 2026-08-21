use tokio::task::JoinError;

use crate::models::ProviderInitError;

use super::runtime::RuntimePreparationError;
use super::startup_preflight::StartupPreflightError;
use super::{DaemonInstanceInner, parts, runner};

pub(super) enum StartupError {
    ProviderGraph(ProviderInitError),
    EagerProviderInit(ProviderInitError),
    ControlRuntime(std::io::Error),
    HighPriorityRuntime(std::io::Error),
    StartupOrchestration(JoinError),
}

impl DaemonInstanceInner {
    pub(super) async fn run_startup_pipeline(&mut self) -> Result<(), StartupError> {
        let runtimes = self
            .run_startup_preflight()
            .await
            .map_err(|err| match err {
                StartupPreflightError::ProviderGraph(err) => StartupError::ProviderGraph(err),
                StartupPreflightError::EagerProviderInit(err) => {
                    StartupError::EagerProviderInit(err)
                }
                StartupPreflightError::Runtime(RuntimePreparationError::Control(err)) => {
                    StartupError::ControlRuntime(err)
                }
                StartupPreflightError::Runtime(RuntimePreparationError::HighPriority(err)) => {
                    StartupError::HighPriorityRuntime(err)
                }
            })?;

        self.standard_runtime = Some(runtimes.standard.clone());

        if let Some(control_runtime) = runtimes.control.as_ref() {
            let startup =
                control_runtime.spawn(runner::spawn_all_services(parts::SpawnAllServicesParts {
                    instances: self.instance_registry.records(),
                    restart_policy: self.restart_policy,
                    running_tasks: self.running_tasks.clone(),
                    resources: self.resources.clone(),
                    diagnostics: self.diagnostics.clone(),
                    isolated_startup_permits: self.isolated_startup_permits.clone(),
                    control_runtime: control_runtime.clone(),
                    standard_runtime: runtimes.standard,
                    high_priority_pool: runtimes
                        .high_priority
                        .as_ref()
                        .map(|_| self.high_priority_runtime_pool.state()),
                    daemon_token: self.cancellation_token.clone(),
                }));

            startup.await.map_err(StartupError::StartupOrchestration)?;
        }

        #[cfg(feature = "diagnostics")]
        super::super::topology_collector::start_topology_collector();

        Ok(())
    }
}
