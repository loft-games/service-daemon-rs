//! Pass case: A provider with default value and env_name compiles successfully.

use service_daemon::provider;

#[derive(Clone)]
#[provider(default = "localhost:5432", env_name = "DATABASE_HOST")]
pub struct DatabaseHost(pub String);

#[derive(Clone)]
#[provider(default = 3306)]
pub struct DatabasePort(pub i32);

fn main() {}
