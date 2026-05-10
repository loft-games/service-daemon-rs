use service_daemon::provider;

#[provider(UnixConnect("/tmp/service-daemon.sock"), eager = true, eager = false)]
pub struct DuplicateEager;

fn main() {}
