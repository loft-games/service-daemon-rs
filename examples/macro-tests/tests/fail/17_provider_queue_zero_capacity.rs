use service_daemon::provider;

#[provider(Queue(String), capacity = 0)]
pub struct ZeroCapacityQueue;

fn main() {}
