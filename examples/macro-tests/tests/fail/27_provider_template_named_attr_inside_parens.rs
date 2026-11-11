//! Fail case: template named attributes belong outside template parentheses.

use service_daemon::provider;

#[provider(Listen("127.0.0.1:8080", env = "LISTEN_ADDR"))]
pub struct BadListen;

fn main() {}
