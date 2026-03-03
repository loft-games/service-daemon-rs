// Test: #[provider(8080, env = "PORT")] with non-String type (i32)
// Verifies that env var parsing works for types that implement FromStr.

use service_daemon::provider;

#[derive(Clone)]
#[provider(8080, env = "TEST_PORT_ENV")]
pub struct TestPort(pub i32);

fn main() {}
