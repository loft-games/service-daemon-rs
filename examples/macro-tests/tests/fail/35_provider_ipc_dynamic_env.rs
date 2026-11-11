use service_daemon::provider;

#[provider(LocalIpcListen("valid-name"), env = format!("LOCAL_IPC_NAME"))]
pub struct DynamicLocalIpcEnv;

fn main() {}
