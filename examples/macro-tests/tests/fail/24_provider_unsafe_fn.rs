use service_daemon::provider;

#[derive(Clone, Default)]
pub struct UnsafeConfig;

#[provider]
pub async unsafe fn unsafe_config_provider() -> UnsafeConfig {
    UnsafeConfig
}

fn main() {}
