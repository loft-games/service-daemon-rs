//! Fail case: Named pipe template named attributes must live outside parentheses.

use service_daemon::provider;

#[provider(NamedPipeListen(r"\\.\pipe\bad", env = "PIPE_NAME"))]
pub struct BadPipeServer;

fn main() {}