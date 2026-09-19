use service_daemon::{provider_contract, provider_impl};

#[derive(Clone)]
#[provider_contract(eager = true)]
struct EagerContract;

#[provider_impl]
async fn eager_contract_impl() -> EagerContract {
    EagerContract
}

#[derive(Clone)]
#[provider_contract(eager = false)]
struct ExplicitLazyContract;

#[provider_impl]
async fn explicit_lazy_contract_impl() -> ExplicitLazyContract {
    ExplicitLazyContract
}

fn main() {}
