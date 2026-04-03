use service_daemon::Provided;
use service_daemon::service;
use std::sync::Arc;

#[derive(Clone)]
struct SnapshotOnly;

impl Provided for SnapshotOnly {
    async fn resolve() -> std::result::Result<Arc<Self>, service_daemon::ProviderInitError> {
        Ok(Arc::new(Self))
    }
}

#[service]
async fn needs_rwlock(_state: Arc<RwLock<SnapshotOnly>>) -> anyhow::Result<()> {
    Ok(())
}

#[service]
async fn needs_mutex(_state: Arc<Mutex<SnapshotOnly>>) -> anyhow::Result<()> {
    Ok(())
}

fn main() {}
