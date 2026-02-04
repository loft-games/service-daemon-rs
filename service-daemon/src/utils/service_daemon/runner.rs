//! Service runner logic for spawning, supervising, and stopping services.

use futures::FutureExt;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, error, info, warn};

use crate::models::{ServiceDescription, ServiceFn, ServiceStatus};
use crate::utils::context::{__run_service_scope, DaemonResources, ServiceIdentity};

use super::policy::RestartPolicy;

/// Spawn a single service with the given restart policy.
pub async fn spawn_service(
    name: String,
    run: ServiceFn,
    watcher: Option<Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>>,
    policy: RestartPolicy,
    running_tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    resources: DaemonResources,
    cancellation_token: CancellationToken,
) {
    let name_for_task = name.clone();
    let resources_clone = resources.clone();
    let handle = tokio::spawn(async move {
        let mut current_delay = policy.initial_delay;
        let name = name_for_task;
        let resources = resources_clone;

        // Spawn dependency watcher if present
        if let Some(watcher) = watcher {
            let n = name.clone();
            let ct = cancellation_token.clone();
            let res = resources.clone();
            tokio::spawn(async move {
                while !ct.is_cancelled() {
                    let reload_signal = res
                        .reload_signals
                        .entry(n.clone())
                        .or_insert_with(|| Arc::new(tokio::sync::Notify::new()))
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

        loop {
            // Determine initial status based on the previous generation
            let initial_status = resources
                .status_plane
                .get(&name)
                .map(|s| s.value().clone())
                .unwrap_or(ServiceStatus::Initializing);

            // Normalizing initial status: if we were reloading, the new instance starts as Restoring.
            // If we were starting or recovering, we preserve that status.
            let start_status = match initial_status {
                ServiceStatus::Initializing => ServiceStatus::Initializing,
                ServiceStatus::Recovering(e) => ServiceStatus::Recovering(e),
                _ => ServiceStatus::Restoring,
            };

            if cancellation_token.is_cancelled() {
                info!(
                    "Service {} received shutdown signal, exiting gracefully",
                    name
                );
                resources
                    .status_plane
                    .insert(name.clone(), ServiceStatus::Terminated);
                resources.status_changed.notify_waiters();
                break;
            }

            info!("Starting service: {} with status {:?}", name, start_status);
            resources
                .status_plane
                .insert(name.clone(), start_status.clone());
            resources.status_changed.notify_waiters();
            let start_time = std::time::Instant::now();

            let span = tracing::info_span!("service", %name);
            let token_clone = cancellation_token.clone();

            // Get or create reload signal
            let reload_signal = resources
                .reload_signals
                .entry(name.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Notify::new()))
                .clone();

            // Create a reload token that we can cancel when a reload is requested.
            // This allows state().await to return NeedReload immediately.
            let reload_token = CancellationToken::new();
            let rt_clone = reload_token.clone();
            let rs_clone = reload_signal.clone();
            let n = name.clone();
            let bridge_task = tokio::spawn(async move {
                rs_clone.notified().await;
                rt_clone.cancel();
                info!(
                    "Supervisor: Reload triggered for {}, notifying service task",
                    n
                );
            });

            let identity =
                ServiceIdentity::new(name.clone(), token_clone.clone(), reload_token.clone());

            // Wrapper to run service in TLS scope and capture errors
            let run_clone = run.clone();
            let token_for_run = token_clone.clone();
            let resources_clone = resources.clone();

            let result = __run_service_scope(identity, resources_clone, || async move {
                std::panic::AssertUnwindSafe(run_clone(token_for_run).instrument(span))
                    .catch_unwind()
                    .await
            })
            .await;

            bridge_task.abort();

            // Determine next status based on outcome
            // Note: Shelf is preserved for recovery; service can decide whether to use it.
            let mut next_status = ServiceStatus::Initializing;

            match result {
                Ok(Ok(_)) => {
                    warn!("Service {} exited normally", name);
                }
                Ok(Err(e)) => {
                    error!("Service {} failed: {:?}", name, e);
                    next_status = ServiceStatus::Recovering(format!("{:?}", e));
                }
                Err(panic) => {
                    let panic_msg = if let Some(s) = panic.downcast_ref::<&str>() {
                        s.to_string()
                    } else if let Some(s) = panic.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "Unknown panic".to_string()
                    };
                    error!("Service {} panicked: {}", name, panic_msg);
                    next_status = ServiceStatus::Recovering(format!("Panic: {}", panic_msg));
                }
            };

            // Check for explicit reload or normal exit
            if reload_token.is_cancelled() {
                info!("Supervisor: Service {} exited after reload signal", name);
                next_status = ServiceStatus::Restoring;
            }

            // Check for cancellation after service exits
            if cancellation_token.is_cancelled() {
                info!("Service {} received shutdown signal, not restarting", name);
                resources
                    .status_plane
                    .insert(name.clone(), ServiceStatus::Terminated);
                resources.status_changed.notify_waiters();
                break;
            }

            info!(
                "Supervisor: Setting next_status for {} to {:?}",
                name, next_status
            );
            resources.status_plane.insert(name.clone(), next_status);
            resources.status_changed.notify_waiters();

            // Reset delay if service ran successfully for long enough
            if start_time.elapsed() >= policy.reset_after {
                current_delay = policy.initial_delay;
            }

            warn!(
                "Restarting service {} in {:.1}s...",
                name,
                current_delay.as_secs_f64()
            );

            // Use select to allow cancellation OR reload during sleep
            tokio::select! {
                _ = tokio::time::sleep(current_delay) => {}
                _ = reload_signal.notified() => {
                    info!("Supervisor: Service {} received immediate reload during restart delay", name);
                    current_delay = Duration::from_millis(0); // Restart immediately
                }
                _ = cancellation_token.cancelled() => {
                    info!("Service {} received shutdown signal during restart delay", name);
                    resources.status_plane.insert(name.clone(), ServiceStatus::Terminated);
                    resources.status_changed.notify_waiters();
                    break;
                }
            }

            // Apply exponential backoff for next restart
            current_delay = policy.next_delay(current_delay);
        }
    });

    running_tasks.lock().await.insert(name, handle);
}

/// Spawn all registered services using wave-based priorities.
///
/// This starts services in descending order of their `priority` value.
/// Services with high priority (e.g. SYSTEM = 100) start first.
pub async fn spawn_all_services(
    services: &[ServiceDescription],
    restart_policy: RestartPolicy,
    running_tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    resources: DaemonResources,
) {
    use std::collections::BTreeMap;

    info!("Beginning wave-based startup sequence...");

    // Group services by priority
    let mut waves: BTreeMap<u8, Vec<&ServiceDescription>> = BTreeMap::new();
    for service in services {
        waves.entry(service.priority).or_default().push(service);
    }

    // Process waves in descending order of priority (u8)
    for (priority, wave_services) in waves.into_iter().rev() {
        info!(
            "Starting wave priority {} ({} services)...",
            priority,
            wave_services.len()
        );

        for service in &wave_services {
            spawn_service(
                service.name.clone(),
                service.run.clone(),
                service.watcher.clone(),
                restart_policy,
                running_tasks.clone(),
                resources.clone(),
                service.cancellation_token.clone(),
            )
            .await;
        }

        // Sync Step: Wait for all services in this wave to become 'Healthy'
        // This ensures Wave 100 actually initialized before Wave 80 starts.
        let start = std::time::Instant::now();
        let mut all_healthy = false;
        while !all_healthy && start.elapsed() < std::time::Duration::from_secs(5) {
            all_healthy = true;
            for service in &wave_services {
                let status = resources
                    .status_plane
                    .get(&service.name)
                    .map(|r| r.value().clone());
                if status != Some(ServiceStatus::Healthy) {
                    all_healthy = false;
                    break;
                }
            }
            if !all_healthy {
                tokio::select! {
                    _ = resources.status_changed.notified() => {}
                    _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
                }
            }
        }

        if !all_healthy {
            warn!(
                "Wave priority {} did not reach 'Healthy' status within 5s, proceeding anyway",
                priority
            );
        }
    }

    info!("All startup waves initiated.");
}

/// Stop all running services gracefully using wave-based priorities.
///
/// This stops services in ascending order of their `priority` value.
/// Services with the same priority are shut down concurrently.
pub async fn stop_all_services(
    services: &[ServiceDescription],
    running_tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    resources: DaemonResources,
    daemon_token: CancellationToken,
) {
    use std::collections::BTreeMap;

    info!("Beginning wave-based graceful shutdown...");

    // Group services by priority
    let mut waves: BTreeMap<u8, Vec<&ServiceDescription>> = BTreeMap::new();
    for service in services {
        waves.entry(service.priority).or_default().push(service);
    }

    let grace_period = std::time::Duration::from_secs(30);

    // Process waves in ascending order of priority (u8)
    for (priority, wave_services) in waves {
        info!(
            "Shutting down wave priority {} ({} services)...",
            priority,
            wave_services.len()
        );

        // 1. Parallel Signal: Cancel all services in this wave
        for service in &wave_services {
            service.cancellation_token.cancel();
            resources
                .status_plane
                .insert(service.name.clone(), ServiceStatus::ShuttingDown);
            resources.status_changed.notify_waiters();
        }

        // 2. Parallel Wait: Wait for all services in this wave to finish
        let mut join_handles = Vec::new();
        for service in wave_services {
            let name = &service.name;
            let handle_opt = {
                let mut guard = running_tasks.lock().await;
                guard.remove(name)
            };
            if let Some(handle) = handle_opt {
                join_handles.push((name.clone(), handle));
            }
        }

        let resources_for_shutdown = resources.clone();
        let mut shutdown_futures = Vec::new();
        for (name, mut handle) in join_handles {
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
                res.status_plane.insert(name, ServiceStatus::Terminated);
                res.status_changed.notify_waiters();
            });
        }
        futures::future::join_all(shutdown_futures).await;
    }

    // Finally, cancel the daemon's own token to signal completion if anyone is watching it
    daemon_token.cancel();
    info!("All shutdown waves completed. ServiceDaemon stopped.");
}
