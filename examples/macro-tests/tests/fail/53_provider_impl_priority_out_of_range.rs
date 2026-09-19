use service_daemon::{provider_contract, provider_impl};

#[derive(Clone)]
#[provider_contract]
struct ContractValue;

#[provider_impl(priority = 256)]
async fn contract_value() -> ContractValue {
    ContractValue
}

fn main() {}
