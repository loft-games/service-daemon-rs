//! Fail case: first-version named pipe templates reject capacity tuning.

use service_daemon::provider;

#[provider(NamedPipeListen(r"\\.\pipe\bad"), capacity = 8)]
pub struct CapacityTunedPipeServer;

fn main() {}