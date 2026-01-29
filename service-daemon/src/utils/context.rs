use tokio::task_local;
use tokio_util::sync::CancellationToken;

task_local! {
    pub static SHUTDOWN_TOKEN: CancellationToken;
}

/// Check if the current service has been signaled to shut down.
///
/// Returns true if shutdown is in progress.
pub fn is_shutdown() -> bool {
    SHUTDOWN_TOKEN.with(|token| token.is_cancelled())
}

/// Wait until the shutdown signal is received.
pub async fn wait_for_shutdown() {
    let token = SHUTDOWN_TOKEN.with(|token| token.clone());
    token.cancelled().await;
}

/// Helper to get the raw CancellationToken if needed.
pub fn token() -> CancellationToken {
    SHUTDOWN_TOKEN.with(|token| token.clone())
}
