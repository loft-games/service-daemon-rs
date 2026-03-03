// Test: env-only provider for non-String type
// Verifies `#[provider(env = "KEY")]` with a non-String inner type compiles.

use service_daemon::provider;

#[derive(Clone)]
#[provider(env = "TEST_TIMEOUT_ENV")]
pub struct TestTimeout(pub String);

fn main() {}
