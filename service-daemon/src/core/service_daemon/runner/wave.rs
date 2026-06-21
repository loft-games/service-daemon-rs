use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::ServiceScheduling;
use crate::core::context::DaemonResources;
use crate::models::{ServiceDescription, ServiceId, ServiceStatus};

use super::super::parts::{
    BodyExecutionLanes, BodyLaneResolver, SpawnAllServicesParts, SpawnServiceParts,
    SupervisorSpawnLane,
};
use super::spawn_service;

/// Helper for wave-based service management.
pub(super) struct ServiceWave<'a> {
    services: Vec<&'a ServiceDescription>,
    priority: u8,
}

impl<'a> ServiceWave<'a> {
    /// Groups services by priority into waves.
    pub(super) fn from_services(
        services: &'a [ServiceDescription],
    ) -> BTreeMap<u8, ServiceWave<'a>> {
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
    pub(super) async fn wait_for_healthy(
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
/// Spawn all registered services using wave-based priorities.
///
/// This starts services in descending order of their `priority` value.
/// Services with high priority (e.g. SYSTEM = 100) start first.
///
/// The `daemon_token` is threaded through to `wait_for_healthy` so that
/// the wave startup sequence can be interrupted immediately if the daemon
/// receives a shutdown signal during startup.
pub(super) async fn spawn_all_services(parts: SpawnAllServicesParts) {
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

    let body_lanes = BodyExecutionLanes {
        standard: standard_runtime,
        high_priority: high_priority_runtime,
    };
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
            if matches!(service.entry.scheduling, ServiceScheduling::HighPriority)
                && body_lanes.high_priority.is_none()
            {
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
            }

            spawn_service(SpawnServiceParts {
                service_id: service.id,
                name: service.name(),
                run: service.entry.wrapper,
                watcher: service.entry.watcher,
                policy: restart_policy,
                scheduling: service.entry.scheduling,
                supervisor_lane: SupervisorSpawnLane::Control(control_runtime.clone()),
                body_lanes: body_lanes.clone(),
                body_lane_resolver: BodyLaneResolver::default(),
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
pub(super) async fn stop_all_services(
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
            let shutting_down = ServiceStatus::ShuttingDown;
            resources
                .status_plane
                .insert(service.id, shutting_down.clone());
            resources
                .runtime_facts
                .record_service_status(service.id, &shutting_down);
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
