use service_daemon::provider_contract;

#[derive(Clone)]
#[provider_contract(eager = true, eager = false)]
struct DuplicateEagerContract;

fn main() {}
