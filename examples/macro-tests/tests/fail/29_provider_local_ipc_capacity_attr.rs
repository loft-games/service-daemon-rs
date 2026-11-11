//! Fail case: LocalIpc templates reject capacity tuning.

use service_daemon::provider;

#[provider(LocalIpcListen("bad"), capacity = 8)]
pub struct CapacityTunedLocalIpcServer;

fn main() {}
