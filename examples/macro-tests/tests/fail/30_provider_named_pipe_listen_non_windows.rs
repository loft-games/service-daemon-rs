//! Fail case: Windows named pipe listener providers are Windows-only.

use service_daemon::provider;

#[provider(NamedPipeListen(r"\\.\pipe\sd-test-listen"))]
pub struct PipeListener;

fn main() {}
