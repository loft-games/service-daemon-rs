use service_daemon::provider;

fn local_ipc_name() -> &'static str {
    "bad"
}

#[provider(LocalIpcListen(local_ipc_name()))]
pub struct DynamicLocalIpcName;

fn main() {}
