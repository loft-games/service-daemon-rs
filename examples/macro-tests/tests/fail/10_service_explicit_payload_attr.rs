//! Fail case: #[service] rejects explicitly annotated `#[payload]` parameters.
//!
//! The `#[payload]` marker is only valid for triggers. Services still require
//! every parameter to be an Arc-based framework-managed dependency.

use service_daemon::service;

#[service]
pub async fn bad_service_with_payload_attr(#[payload] data: String) -> anyhow::Result<()> {
    let _ = data;
    Ok(())
}

fn main() {}
