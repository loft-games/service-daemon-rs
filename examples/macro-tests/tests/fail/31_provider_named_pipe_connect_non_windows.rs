//! Fail case: Windows named pipe client providers are Windows-only.

use service_daemon::provider;

#[provider(NamedPipeConnect(r"\\.\pipe\sd-test-connect"))]
pub struct PipeClient;

fn main() {}
