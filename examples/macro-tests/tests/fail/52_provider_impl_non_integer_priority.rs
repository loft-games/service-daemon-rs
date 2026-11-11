use service_daemon::{provider_contract, provider_impl};

#[derive(Clone)]
#[provider_contract]
struct ContractValue;

#[provider_impl(priority = "high")]
async fn contract_value() -> ContractValue {
    ContractValue
}

fn main() {}
