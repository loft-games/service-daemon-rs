//! Pass case: Provider with env-only (no default) and env with fallback default.

use service_daemon::provider;

/// Environment-only provider: will panic at runtime if `API_KEY` is not set.
/// This test only verifies that the macro expansion _compiles_.
#[derive(Clone)]
#[provider(env = "API_KEY")]
pub struct ApiKey(pub String);

/// Provider with both default and env override.
#[derive(Clone)]
#[provider("https://api.example.com", env = "API_BASE_URL")]
pub struct ApiBaseUrl(pub String);

fn main() {}
