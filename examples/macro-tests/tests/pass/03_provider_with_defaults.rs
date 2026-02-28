//! Pass case: A provider with default value and env compiles successfully.

use service_daemon::provider;

#[derive(Clone)]
#[provider("localhost:5432", env = "DATABASE_HOST")]
pub struct DatabaseHost(pub String);

#[derive(Clone)]
#[provider(3306)]
pub struct DatabasePort(pub i32);

fn main() {}
