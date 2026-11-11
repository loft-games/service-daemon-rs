//! Provider templates for the Windows named pipe example.

use service_daemon::provider;

pub const EXAMPLE_NAMED_PIPE_NAME: &str = r"\\.\pipe\service-daemon-rs-named-pipe-example";
pub const EXAMPLE_NAMED_PIPE_ENV: &str = "SERVICE_DAEMON_RS_NAMED_PIPE_EXAMPLE_NAME";

#[provider(
    NamedPipeListen(EXAMPLE_NAMED_PIPE_NAME),
    env = EXAMPLE_NAMED_PIPE_ENV
)]
pub struct ExampleNamedPipeListener;

#[provider(
    NamedPipeConnect(EXAMPLE_NAMED_PIPE_NAME),
    env = EXAMPLE_NAMED_PIPE_ENV
)]
pub struct ExampleNamedPipeConnector;
