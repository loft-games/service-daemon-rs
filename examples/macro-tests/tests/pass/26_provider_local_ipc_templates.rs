//! Pass case: cross-platform LocalIpc provider templates compile.

use service_daemon::provider;

const IPC_NAME: &str = "service-daemon-rs-macro-pass";
const IPC_ENV: &str = "SERVICE_DAEMON_RS_MACRO_LOCAL_IPC_NAME";

#[provider(LocalIpcListen("service-daemon-rs-macro-pass"))]
pub struct IpcServer;

#[provider(
    LocalIpcConnect(IPC_NAME),
    env = IPC_ENV,
    eager = true
)]
pub struct IpcClient;

fn main() {}
