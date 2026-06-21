use crate::models::{Result as ServiceResult, ServiceError, ServiceScheduling};

use super::runtime::RuntimePreparationError;
use super::startup_preflight::StartupPreflightError;
use super::{RestartPolicy, ServiceDaemon, parts, runner};

impl ServiceDaemon {
    pub(super) async fn run_simulation_startup(
        &mut self,
        test_policy: RestartPolicy,
    ) -> ServiceResult<()> {
        let daemon_token = self.cancellation_token.clone();

        let runtimes = self
            .run_startup_preflight()
            .await
            .map_err(|err| match err {
                StartupPreflightError::ProviderGraph(err) => ServiceError::InternalError(format!(
                    "provider dependency graph validation failed: {err}"
                )),
                StartupPreflightError::EagerProviderInit(err) => ServiceError::InternalError(
                    format!("eager provider initialization failed: {err}"),
                ),
                StartupPreflightError::Runtime(
                    RuntimePreparationError::Control(err)
                    | RuntimePreparationError::HighPriority(err),
                ) => ServiceError::InternalError(err.to_string()),
            })?;

        if let Some(control_runtime) = runtimes.control.as_ref() {
            let body_lanes = parts::BodyExecutionLanes {
                standard: runtimes.standard.clone(),
                high_priority: runtimes.high_priority.clone(),
            };
            for service in &self.services {
                if matches!(service.entry.scheduling, ServiceScheduling::HighPriority)
                    && body_lanes.high_priority.is_none()
                {
                    return Err(ServiceError::InternalError(format!(
                        "HighPriority service '{}' is missing the shared high-priority runtime",
                        service.name()
                    )));
                }

                runner::spawn_service(parts::SpawnServiceParts {
                    service_id: service.id,
                    name: service.name(),
                    run: service.entry.wrapper,
                    watcher: service.entry.watcher,
                    policy: test_policy,
                    scheduling: service.entry.scheduling,
                    supervisor_lane: parts::SupervisorSpawnLane::Control(control_runtime.clone()),
                    body_lanes: body_lanes.clone(),
                    body_lane_resolver: parts::BodyLaneResolver::default(),
                    running_tasks: self.running_tasks.clone(),
                    resources: self.resources.clone(),
                    diagnostics: self.diagnostics.clone(),
                    isolated_startup_permits: self.isolated_startup_permits.clone(),
                    cancellation_token: service.cancellation_token.clone(),
                    daemon_token: daemon_token.clone(),
                })
                .await;
            }
        }

        Ok(())
    }
}
