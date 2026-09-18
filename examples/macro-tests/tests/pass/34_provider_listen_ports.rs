use service_daemon::provider;

#[provider(Listen(8080), env = "API_BIND", eager = true)]
struct Numeric;
#[provider(Listen("8080"))]
struct Text;
#[provider(Listen(0))]
struct Ephemeral;
#[provider(Listen(65535))]
struct Maximum;
#[provider(Listen("127.0.0.1:8080"))]
struct Loopback;
#[provider(Listen("[::]:8080"))]
struct Ipv6;

fn main() {}
