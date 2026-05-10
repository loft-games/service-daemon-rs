use service_daemon::provider;

#[provider(UnixConnect("/tmp/service-daemon.sock"), eager = yes)]
pub struct MalformedEager;

fn main() {}
