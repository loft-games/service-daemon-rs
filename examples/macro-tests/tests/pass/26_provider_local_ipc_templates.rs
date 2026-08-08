//! Pass case: cross-platform LocalIpc provider templates compile.

use service_daemon::provider;

#[provider(LocalIpcListen("service-daemon-rs-macro-pass"))]
pub struct IpcServer;

#[provider(
    LocalIpcConnect("service-daemon-rs-macro-pass"),
    env = "SERVICE_DAEMON_RS_MACRO_LOCAL_IPC_NAME",
    eager = true
)]
pub struct IpcClient;

fn main() {}
