use service_daemon::TT::*;
use service_daemon::trigger;
use std::sync::Arc;

#[derive(Clone)]
struct NotWatchable;

// Intentionally missing `WatchableProvided`.
// We implement `Provided` + `ManagedProvided` so the compile error is
// specifically about watchability.
impl service_daemon::Provided for NotWatchable {
    async fn resolve() -> std::result::Result<Arc<Self>, service_daemon::ProviderInitError> {
        Ok(Arc::new(Self))
    }
}

impl service_daemon::ManagedProvided for NotWatchable {
    async fn resolve_rwlock(
    ) -> std::result::Result<Arc<service_daemon::core::managed_state::RwLock<Self>>, service_daemon::ProviderInitError> {
        unimplemented!()
    }

    async fn resolve_mutex(
    ) -> std::result::Result<Arc<service_daemon::core::managed_state::Mutex<Self>>, service_daemon::ProviderInitError> {
        unimplemented!()
    }

    async fn resolve_managed() -> std::result::Result<Arc<Self>, service_daemon::ProviderError> {
        unimplemented!()
    }
}

#[trigger(Watch(NotWatchable))]
async fn watch_state(_snapshot: Arc<NotWatchable>) -> anyhow::Result<()> {
    Ok(())
}

fn main() {}
