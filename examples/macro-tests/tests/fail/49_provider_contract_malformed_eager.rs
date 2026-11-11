use service_daemon::provider_contract;

#[derive(Clone)]
#[provider_contract(eager = "true")]
struct MalformedEagerContract;

fn main() {}
