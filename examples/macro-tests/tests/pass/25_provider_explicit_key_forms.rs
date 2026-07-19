//! Pass case: Explicit `default = ...` and `template = ...` provider heads are
//! accepted as equivalents to the positional provider sugar.

use service_daemon::provider;

#[derive(Clone)]
#[provider(default = 8080)]
pub struct Port(pub u16);

#[derive(Clone)]
#[provider(default = "localhost:5432", env = "DATABASE_HOST")]
pub struct DatabaseHost(pub String);

#[provider(template = Queue(String))]
pub struct JobQueue;

#[provider(template = Queue(i32), capacity = 32)]
pub struct SizedJobQueue;

#[provider(template = Listen("127.0.0.1:0"), env = "LISTEN_ADDR", eager = true)]
pub struct ApiListener;

fn main() {}
