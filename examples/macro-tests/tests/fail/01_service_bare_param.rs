//! Fail case: #[service] rejects bare dependency-like parameters.
//!
//! Services do not have payload parameters. Every service dependency must be
//! declared as `Arc<T>`, `Arc<RwLock<T>>`, or `Arc<Mutex<T>>`.
//! A bare `i32` therefore fails as an unsupported service signature.

use service_daemon::service;

#[service]
pub async fn bad_service(port: i32) -> anyhow::Result<()> {
    Ok(())
}

fn main() {}
