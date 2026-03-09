//! Fail case: #[trigger] rejects multiple payload parameters.
//!
//! A trigger may accept exactly one payload parameter. Additional bare parameters
//! are ambiguous and should be modeled as Arc-wrapped dependencies instead.

use service_daemon::trigger;

#[trigger(Queue(String))]
pub async fn too_many_payloads(first: String, second: String) -> anyhow::Result<()> {
    let _ = (first, second);
    Ok(())
}

fn main() {}
