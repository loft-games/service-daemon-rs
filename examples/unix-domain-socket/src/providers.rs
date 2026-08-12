use service_daemon::provider;

pub const EXAMPLE_UNIX_DOMAIN_SOCKET_PATH: &str =
    "/tmp/service-daemon-unix-domain-socket-example.socket";

#[provider(UnixListen(EXAMPLE_UNIX_DOMAIN_SOCKET_PATH))]
pub struct ExampleUnixDomainSocketListener;

#[provider(UnixConnect(EXAMPLE_UNIX_DOMAIN_SOCKET_PATH))]
pub struct ExampleUnixDomainSocketConnector;
