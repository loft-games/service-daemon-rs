//! Fail case: #[service] rejects unknown attributes.
//!
//! Only `priority` and `tags` are valid #[service] attributes.
//! Using an unknown attribute like `timeout` should produce a compile error.

use service_daemon::service;

#[service(timeout = 30)]
pub async fn bad_attr_service() -> anyhow::Result<()> {
    Ok(())
}

fn main() {}
