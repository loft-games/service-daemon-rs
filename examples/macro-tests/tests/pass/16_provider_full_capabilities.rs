//! Pass case: a normal `#[provider]` should provide snapshot, managed, and watchable capabilities.

use service_daemon::TT::*;
use service_daemon::{provider, service, trigger};

#[derive(Clone, Default)]
#[provider]
pub struct FullState {
    pub value: i32,
}

#[service]
pub async fn snapshot_service(_state: Arc<FullState>) -> anyhow::Result<()> {
    Ok(())
}

#[service]
pub async fn rwlock_service(_state: Arc<RwLock<FullState>>) -> anyhow::Result<()> {
    Ok(())
}

#[service]
pub async fn mutex_service(_state: Arc<Mutex<FullState>>) -> anyhow::Result<()> {
    Ok(())
}

#[trigger(Watch(FullState))]
pub async fn watch_trigger(_snapshot: Arc<FullState>) -> anyhow::Result<()> {
    Ok(())
}

fn main() {}
