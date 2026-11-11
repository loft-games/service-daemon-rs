//! Fail case: first-version named pipe templates reject tuning attributes.

use service_daemon::provider;

#[provider(NamedPipeConnect(r"\\.\pipe\bad"), pipe_mode = "message")]
pub struct TunedPipeClient;

fn main() {}