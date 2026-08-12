//! Services that demonstrate on-demand runtime instance management.
//!
//! The `template` module contains service definitions that are selected by the
//! daemon but do not auto-start. The controller service resolves a
//! daemon-bound `ServiceHandle` and periodically creates short-lived runtime
//! instances from that definition.

pub mod template;

use crate::providers::WorkerService;
use service_daemon::{done, service, sleep};
use std::time::Duration;
use tracing::{error, info, warn};

#[service(tags = ["on-demand"], priority = 80)]
pub async fn worker_controller(worker: Arc<WorkerService>) -> anyhow::Result<()> {
    info!(
        service = worker.name(),
        instances = worker.instances().len(),
        "selected worker definition is available without auto-started instances"
    );
    done();

    while !service_daemon::is_shutdown() {
        if let Err(err) = run_worker_once(&worker).await {
            error!(error = ?err, "on-demand worker lifecycle failed");
            if !sleep(Duration::from_secs(1)).await {
                break;
            }
            continue;
        }

        if !sleep(Duration::from_secs(2)).await {
            break;
        }
    }

    Ok(())
}

pub async fn run_worker_once(worker_service: &WorkerService) -> anyhow::Result<()> {
    info!(
        service = worker_service.name(),
        instances = worker_service.instances().len(),
        "starting one on-demand worker lifecycle"
    );

    // start is the create-and-run convenience path for common on-demand usage.
    // When callers need to decide later when execution should begin, use
    // `worker_service.create().await` to get a ServiceInstanceHandle without
    // running it, then call `instance.start().await` when ready.
    let worker = match worker_service.start().await {
        Ok(worker) => worker,
        Err(error) => {
            error!(error = ?error, "failed to create and start worker instance");
            return Err(error.into());
        }
    };
    info!(
        instance = %worker.instance_id(),
        "started worker instance"
    );

    if !sleep(Duration::from_millis(500)).await {
        return Ok(());
    }

    // stop requests shutdown and waits for the instance task to finish.
    match worker.stop().await {
        Ok(true) => {}
        Ok(false) => {
            warn!(
                instance = %worker.instance_id(),
                "worker instance was not stopped because it no longer belongs to this daemon"
            );
            return Ok(());
        }
        Err(error) => {
            error!(instance = %worker.instance_id(), error = ?error, "failed to stop worker instance");
            worker.request_stop();
            return Err(error.into());
        }
    }
    info!(instance = %worker.instance_id(), "stopped worker instance");

    // remove clears daemon-local runtime state after the instance is stopped.
    match worker.remove().await {
        Ok(true) => {}
        Ok(false) => {
            warn!(
                instance = %worker.instance_id(),
                "worker instance runtime state was not removed because it no longer belongs to this daemon"
            );
            return Ok(());
        }
        Err(error) => {
            error!(instance = %worker.instance_id(), error = ?error, "failed to remove worker instance runtime state");
            return Err(error.into());
        }
    }
    info!(
        instance = %worker.instance_id(),
        "removed worker instance runtime state"
    );

    Ok(())
}
