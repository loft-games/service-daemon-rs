//! Pass case: Windows named pipe provider templates compile on Windows.

#[cfg(windows)]
mod windows_named_pipe_templates {
    use service_daemon::provider;

    #[provider(NamedPipeListen(r"\\.\pipe\service-daemon-rs-macro-pass"))]
    pub struct PipeServer;

    #[provider(
        NamedPipeConnect(r"\\.\pipe\service-daemon-rs-macro-pass"),
        env = "SERVICE_DAEMON_RS_MACRO_PIPE_NAME",
        eager = true
    )]
    pub struct PipeClient;
}

fn main() {}