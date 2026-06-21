//! Pass case: Trigger macros with Queue, Event, and Watch templates compile.

use service_daemon::TT::*;
use service_daemon::{provider, trigger};

#[provider(Notify)]
pub struct MySignal;

#[provider(Queue(String))]
pub struct MyQueue;

#[derive(Debug, Clone)]
#[provider]
pub struct WatchableState {
    pub value: i32,
}

impl Default for WatchableState {
    fn default() -> Self {
        Self { value: 0 }
    }
}

#[trigger(Event(MySignal))]
pub async fn on_signal() -> anyhow::Result<()> {
    Ok(())
}

#[trigger(Event(MySignal))]
#[allow(dead_code, sync_handler, clippy::too_many_arguments)]
#[doc = "Intentionally synchronous trigger used to verify attribute preservation."]
pub fn on_signal_sync() -> anyhow::Result<()> {
    Ok(())
}

#[trigger(Queue(MyQueue))]
pub async fn on_queue_item(item: String) -> anyhow::Result<()> {
    tracing::info!("Received: {}", item);
    Ok(())
}

#[trigger(Watch(WatchableState))]
pub async fn on_state_changed(snapshot: Arc<WatchableState>) -> anyhow::Result<()> {
    tracing::info!("State changed: {}", snapshot.value);
    Ok(())
}

fn main() {}
