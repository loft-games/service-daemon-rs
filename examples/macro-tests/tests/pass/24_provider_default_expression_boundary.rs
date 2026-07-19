//! Pass case: non-template identifiers remain default expressions.

use service_daemon::provider;

const DEFAULT_PORT: u16 = 8080;

#[derive(Clone)]
#[provider(DEFAULT_PORT)]
pub struct Port(pub u16);

fn main() {}
