// Value and env-backed providers.
// Inject any of these as `Arc<T>` into a service/trigger/provider.
use service_daemon::provider;

// Constant default.
#[provider(8080)]
pub struct Port(pub i32);

// String default (literal auto-expands to .to_owned()).
#[provider("mysql://localhost")]
pub struct DbUrl(pub String);

// String field sourced from an env var, with a fallback default.
#[provider("localhost:5432", env = "DATABASE_HOST")]
pub struct DatabaseHost(pub String);

// Non-String field: the env var is parsed via `.parse()`.
#[provider(8080, env = "PORT")]
pub struct ConfiguredPort(pub i32);

// Env-only: initialization fails if API_KEY is not set.
#[provider(env = "API_KEY")]
pub struct ApiKey(pub String);
