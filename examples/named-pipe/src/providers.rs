//! Demonstrates Windows named pipe listener and connector provider templates.

use service_daemon::provider;

pub const EXAMPLE_NAMED_PIPE_NAME: &str = r"\\.\pipe\service-daemon-named-pipe-example";
pub const EXAMPLE_NAMED_PIPE_NAME_ENV: &str = "SERVICE_DAEMON_EXAMPLE_NAMED_PIPE_NAME";

#[provider(
    NamedPipeListen(r"\\.\pipe\service-daemon-named-pipe-example"),
    env = "SERVICE_DAEMON_EXAMPLE_NAMED_PIPE_NAME"
)]
pub struct ExampleNamedPipeListener;

#[provider(
    NamedPipeConnect(r"\\.\pipe\service-daemon-named-pipe-example"),
    env = "SERVICE_DAEMON_EXAMPLE_NAMED_PIPE_NAME"
)]
pub struct ExampleNamedPipeConnector;
