//! Pass case: Windows named pipe provider templates compile on Windows.

#[cfg(windows)]
mod windows_named_pipe_templates {
    use service_daemon::provider;

    const PIPE_NAME: &str = r"\\.\pipe\service-daemon-rs-macro-pass";
    const PIPE_ENV: &str = "SERVICE_DAEMON_RS_MACRO_PIPE_NAME";

    #[provider(NamedPipeListen(r"\\.\pipe\service-daemon-rs-macro-pass"))]
    pub struct PipeServer;

    #[provider(
        NamedPipeConnect(PIPE_NAME),
        env = PIPE_ENV,
        eager = true
    )]
    pub struct PipeClient;
}

fn main() {}
