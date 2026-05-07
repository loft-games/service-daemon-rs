use service_daemon::provider;

pub const EXAMPLE_UNIX_DOMAIN_SOCKET_PATH: &str =
    "/tmp/service-daemon-unix-domain-socket-example.socket";

#[provider(UnixListen("/tmp/service-daemon-unix-domain-socket-example.socket"))]
pub struct ExampleUnixDomainSocketListener;

#[provider(UnixConnect("/tmp/service-daemon-unix-domain-socket-example.socket"))]
pub struct ExampleUnixDomainSocketConnector;
