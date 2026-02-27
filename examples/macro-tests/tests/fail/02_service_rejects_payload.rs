//! Fail case: #[service] rejects payload parameters.
//!
//! Services do NOT support event payloads. Only triggers can
//! accept payloads. Using a non-Arc parameter in a service
//! should produce a clear error message.

use service_daemon::service;

#[service]
pub async fn bad_payload_service(data: String) -> anyhow::Result<()> {
    Ok(())
}

fn main() {}
