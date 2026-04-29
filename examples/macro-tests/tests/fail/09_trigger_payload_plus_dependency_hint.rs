//! Fail case: #[trigger] reports clearer guidance for unsupported dependency parameters.
//!
//! Trigger handlers may use one bare payload parameter plus Arc-wrapped
//! dependencies. A second bare parameter should fail with dependency guidance.

use service_daemon::{provider, trigger};

#[provider(Queue(String))]
pub struct TestQueue;

#[trigger(Queue(TestQueue))]
pub async fn payload_plus_bare_dependency(payload: String, retries: usize) -> anyhow::Result<()> {
    let _ = (payload, retries);
    Ok(())
}

fn main() {}
