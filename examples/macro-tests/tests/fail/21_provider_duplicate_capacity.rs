use service_daemon::provider;

#[provider(Queue(String), capacity = 10, capacity = 20)]
pub struct DuplicateCapacity;

fn main() {}
