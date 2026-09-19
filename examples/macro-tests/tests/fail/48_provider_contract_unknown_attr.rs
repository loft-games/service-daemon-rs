use service_daemon::provider_contract;

#[derive(Clone)]
#[provider_contract(capacity = 4)]
struct UnknownContractAttribute;

fn main() {}
