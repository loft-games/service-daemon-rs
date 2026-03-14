use service_daemon::TT::*;
use service_daemon::trigger;
use std::sync::Arc;

#[derive(Clone)]
struct NotWatchable;

// Intentionally missing `WatchableProvided`.
// We implement `Provided` + `ManagedProvided` so the compile error is
// specifically about watchability.
impl service_daemon::Provided for NotWatchable {
    async fn resolve() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl service_daemon::ManagedProvided for NotWatchable {
    async fn resolve_rwlock() -> Arc<service_daemon::core::managed_state::RwLock<Self>> {
        unimplemented!()
    }

    async fn resolve_mutex() -> Arc<service_daemon::core::managed_state::Mutex<Self>> {
        unimplemented!()
    }
}

#[trigger(Watch(NotWatchable))]
async fn watch_state(_snapshot: Arc<NotWatchable>) -> anyhow::Result<()> {
    Ok(())
}

fn main() {}
