//! Fail case: #[trigger] rejects multiple payload parameters.
//!
//! A trigger may accept exactly one payload parameter. Additional bare parameters
//! are ambiguous and should be modeled as Arc-wrapped dependencies instead.

use service_daemon::{provider, trigger};

#[provider(Queue(String))]
pub struct TestQueue;

#[trigger(Queue(TestQueue))]
pub async fn too_many_payloads(first: String, second: String) -> anyhow::Result<()> {
    let _ = (first, second);
    Ok(())
}

fn main() {}
