//! Fail case: LocalIpc logical names reject platform path syntax.

use service_daemon::provider;

#[provider(LocalIpcConnect("bad/name"))]
pub struct InvalidLocalIpcClient;

fn main() {}
