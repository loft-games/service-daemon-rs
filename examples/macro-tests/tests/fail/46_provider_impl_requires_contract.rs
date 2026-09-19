use service_daemon::provider_impl;

#[derive(Clone)]
pub struct NotAContract;

#[provider_impl]
pub async fn missing_contract() -> NotAContract {
    NotAContract
}

fn main() {}
