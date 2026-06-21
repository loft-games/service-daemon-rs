// Trigger handlers for each host family. The Target in #[trigger(Host(Target))]
// is the #[provider] type that supplies the event source.
use std::sync::Arc;
use service_daemon::trigger;

// Signal: fires when MyNotifier (a #[provider(Notify)]) is raised. No payload.
#[trigger(Event(MyNotifier))]
async fn on_event(db: Arc<DbPool>) -> anyhow::Result<()> {
    db.record_event().await?;
    Ok(())
}

// Queue: payload is the published item, by value. Matches #[provider(Queue(String))].
#[trigger(Queue(TaskQueue))]
async fn on_queue_item(item: String) -> anyhow::Result<()> {
    println!("received: {item}");
    Ok(())
}

// Cron: fires on the schedule supplied by CleanupSchedule (a provider yielding the
// cron expression String). No payload.
#[trigger(Cron(CleanupSchedule))]
async fn on_cron_tick() -> anyhow::Result<()> {
    Ok(())
}

// Watch: fires when MetricsData changes; receives the new snapshot as Arc<T>.
// priority/scheduling/tags work the same as on #[service].
#[trigger(Watch(MetricsData), priority = 80)]
async fn on_metrics_changed(snapshot: Arc<MetricsData>) -> anyhow::Result<()> {
    println!("metrics updated: {snapshot:?}");
    Ok(())
}
