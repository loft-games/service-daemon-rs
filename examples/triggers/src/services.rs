//! Source services -- the "stone throwers" of the system.
//!
//! These services generate events that flow through the trigger system.
//! Provider methods are called directly via DI-injected instances.

use crate::providers::{ComplexJob, JobQueue, TaskQueue, UserNotifier, WorkerQueue};
use service_daemon::service;
use std::time::Duration;

/// Periodically fires events to all provider types using explicit DI.
///
/// Each provider is injected as an `Arc<T>` parameter, making dependencies
/// visible in the function signature. Calling provider instance methods
/// (e.g. `notifier.notify()`, `tasks.push(...)`) triggers the
/// corresponding downstream handlers.
#[service]
pub async fn event_producer(
    notifier: Arc<UserNotifier>,
    tasks: Arc<TaskQueue>,
    workers: Arc<WorkerQueue>,
    jobs: Arc<JobQueue>,
) -> anyhow::Result<()> {
    service_daemon::done();

    let mut counter = 0u64;
    while !service_daemon::is_shutdown() {
        // Signal: fire the notifier (drives on_tick, on_user_notified, sync_notify_trigger)
        notifier.notify();

        // Broadcast Queue: all handlers receive every message
        let _ = tasks.push(format!("Broadcast #{}", counter));

        // Worker Queue: messages are broadcast to all subscribed handlers
        let _ = workers.push(format!("LB Work #{}", counter));

        // Complex payload queue
        let _ = jobs.push(ComplexJob {
            id: counter,
            data: format!("Complex Data #{}", counter),
        });

        tracing::info!(counter, "Event batch published");
        counter += 1;

        if !service_daemon::sleep(Duration::from_secs(5)).await {
            break;
        }
    }

    Ok(())
}
