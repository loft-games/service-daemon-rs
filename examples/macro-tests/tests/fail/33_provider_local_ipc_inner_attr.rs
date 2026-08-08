//! Fail case: LocalIpc template named attributes belong outside template parentheses.

use service_daemon::provider;

#[provider(LocalIpcListen("bad", env = "LOCAL_IPC_NAME"))]
pub struct BadLocalIpc;

fn main() {}
