//! Provider templates for the cross-platform local IPC example.

use service_daemon::provider;

pub const EXAMPLE_LOCAL_IPC_NAME: &str = "service-daemon-rs-local-ipc-example";
pub const EXAMPLE_LOCAL_IPC_ENV: &str = "SERVICE_DAEMON_RS_LOCAL_IPC_EXAMPLE_NAME";

#[provider(
    LocalIpcListen(EXAMPLE_LOCAL_IPC_NAME),
    env = EXAMPLE_LOCAL_IPC_ENV
)]
pub struct ExampleLocalIpcListener;

#[provider(
    LocalIpcConnect(EXAMPLE_LOCAL_IPC_NAME),
    env = EXAMPLE_LOCAL_IPC_ENV
)]
pub struct ExampleLocalIpcConnector;
