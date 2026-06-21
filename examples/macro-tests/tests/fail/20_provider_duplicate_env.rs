use service_daemon::provider;

#[provider("fallback", env = "FIRST", env = "SECOND")]
pub struct DuplicateEnv(pub String);

fn main() {}
