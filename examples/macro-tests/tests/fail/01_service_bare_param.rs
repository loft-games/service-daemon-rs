//! Fail case: #[service] rejects non-Arc parameter (bare type).
//!
//! A service function MUST wrap all dependencies in Arc<T>.
//! Using a bare `i32` should produce a compile-time error from the macro.

use service_daemon::service;

#[service]
pub async fn bad_service(port: i32) -> anyhow::Result<()> {
    Ok(())
}

fn main() {}
