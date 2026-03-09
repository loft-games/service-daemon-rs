//! Fail case: #[service] rejects bare parameters even if the author thinks of them as payload-like.
//!
//! Payloads belong only to `#[trigger]`. In `#[service]`, a bare parameter like
//! `String` is still an invalid signature because service parameters must be
//! Arc-based framework-managed dependencies.

use service_daemon::service;

#[service]
pub async fn bad_payload_service(data: String) -> anyhow::Result<()> {
    Ok(())
}

fn main() {}
