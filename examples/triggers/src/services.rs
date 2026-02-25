//! Source services — the "stone throwers" of the system.
//!
//! These services generate events (publish) that flow through the trigger
//! system, demonstrating full traceability via `publish()`.

use service_daemon::{publish, service};
use std::time::Duration;

/// Periodically publishes events to all provider types, demonstrating
/// full traceability via `service_daemon::publish()`.
///
/// Each publish call automatically captures the current ServiceId and
/// generates a unique `message_id` visible in structured logs, enabling
/// full end-to-end event traceability.
#[service]
pub async fn event_producer() -> anyhow::Result<()> {
    service_daemon::done();

    let mut counter = 0u64;
    while !service_daemon::is_shutdown() {
        // Signal: fire the notifier (drives on_tick, on_user_notified, sync_notify_trigger)
        publish("notify", || async {
            crate::providers::UserNotifier::notify().await;
        })
        .await;

        // Broadcast Queue: all handlers receive every message
        publish("broadcast_task", || async {
            let _ = crate::providers::TaskQueue::push(format!("Broadcast #{}", counter)).await;
        })
        .await;

        // Load-Balancing Queue: one handler per message
        publish("lb_work", || async {
            let _ = crate::providers::WorkerQueue::push(format!("LB Work #{}", counter)).await;
        })
        .await;

        // Complex payload queue
        publish("complex_job", || async {
            let _ = crate::providers::JobQueue::push(crate::providers::ComplexJob {
                id: counter,
                data: format!("Complex Data #{}", counter),
            })
            .await;
        })
        .await;

        tracing::info!(counter, "Event batch published");
        counter += 1;

        if !service_daemon::sleep(Duration::from_secs(5)).await {
            break;
        }
    }

    Ok(())
}
