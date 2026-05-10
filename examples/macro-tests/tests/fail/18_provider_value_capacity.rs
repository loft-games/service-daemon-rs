use service_daemon::provider;

#[provider("fallback", capacity = 10)]
pub struct ValueCapacity(pub String);

fn main() {}
