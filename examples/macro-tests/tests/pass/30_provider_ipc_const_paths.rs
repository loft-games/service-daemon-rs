//! Pass case: IPC provider endpoint and env arguments accept const string paths.

use service_daemon::provider;

const LOCAL_IPC_NAME: &str = "service-daemon-rs-macro-const-path";
const LOCAL_IPC_ENV: &str = "SERVICE_DAEMON_RS_MACRO_LOCAL_IPC_CONST_PATH";

#[provider(LocalIpcListen(crate::LOCAL_IPC_NAME), env = crate::LOCAL_IPC_ENV)]
pub struct LocalIpcServer;

#[provider(LocalIpcConnect(LOCAL_IPC_NAME), env = LOCAL_IPC_ENV)]
pub struct LocalIpcClient;

#[cfg(unix)]
mod unix_templates {
    use service_daemon::provider;

    const SOCKET_PATH: &str = "/tmp/service-daemon-rs-macro-const-path.sock";
    const SOCKET_ENV: &str = "SERVICE_DAEMON_RS_MACRO_UNIX_CONST_PATH";

    #[provider(UnixListen(self::SOCKET_PATH), env = self::SOCKET_ENV)]
    pub struct UnixServer;

    #[provider(UnixConnect(SOCKET_PATH), env = SOCKET_ENV)]
    pub struct UnixClient;
}

#[cfg(windows)]
mod windows_templates {
    use service_daemon::provider;

    const PIPE_NAME: &str = r"\\.\pipe\service-daemon-rs-macro-const-path";
    const PIPE_ENV: &str = "SERVICE_DAEMON_RS_MACRO_NAMED_PIPE_CONST_PATH";

    #[provider(NamedPipeListen(self::PIPE_NAME), env = self::PIPE_ENV)]
    pub struct PipeServer;

    #[provider(NamedPipeConnect(PIPE_NAME), env = PIPE_ENV)]
    pub struct PipeClient;
}

fn main() {}
