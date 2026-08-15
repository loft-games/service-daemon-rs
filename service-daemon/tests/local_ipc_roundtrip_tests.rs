// End-to-end tests: LocalIpcListen and LocalIpcConnect cooperate through a
// platform-mapped local IPC endpoint.

use service_daemon::{ManagedProvided, ProviderError, provider};
use std::ffi::OsString;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex as AsyncMutex;

const REQUEST_PAYLOAD: &[u8] = b"\x00local-ipc-request\xff";
const RESPONSE_PAYLOAD: &[u8] = b"\xfeok\x00response";

static IPC_COUNTER: AtomicU64 = AtomicU64::new(0);
static ENV_VAR_LOCK: LazyLock<AsyncMutex<()>> = LazyLock::new(|| AsyncMutex::new(()));

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // Rust 2024 marks environment mutation unsafe because it is process-global.
        unsafe {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

fn set_local_ipc_name(key: &'static str, label: &str) -> (EnvVarGuard, String) {
    let counter = IPC_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = format!(
        "service-daemon-rs-local-ipc-{label}-{}-{counter}",
        std::process::id()
    );
    let previous = std::env::var_os(key);
    // Rust 2024 marks environment mutation unsafe because it is process-global.
    unsafe {
        std::env::set_var(key, &name);
    }
    (EnvVarGuard { key, previous }, name)
}

fn set_raw_env_value(key: &'static str, value: &str) -> EnvVarGuard {
    let previous = std::env::var_os(key);
    // Rust 2024 marks environment mutation unsafe because it is process-global.
    unsafe {
        std::env::set_var(key, value);
    }
    EnvVarGuard { key, previous }
}

#[cfg(unix)]
mod unix_tests {
    use super::*;

    const ROUNDTRIP_ENV_VAR: &str = "SERVICE_DAEMON_RS_LOCAL_IPC_UNIX_ROUNDTRIP_NAME_0CFD61B9";
    const OVERRIDE_ENV_VAR: &str = "SERVICE_DAEMON_RS_LOCAL_IPC_UNIX_OVERRIDE_NAME_A49E8522";
    const INVALID_LISTEN_ENV_VAR: &str =
        "SERVICE_DAEMON_RS_LOCAL_IPC_UNIX_INVALID_LISTEN_NAME_C64173F7";
    const INVALID_CONNECT_ENV_VAR: &str =
        "SERVICE_DAEMON_RS_LOCAL_IPC_UNIX_INVALID_CONNECT_NAME_F23F5C14";

    #[derive(Debug)]
    #[provider(
        LocalIpcListen("service-daemon-rs-local-ipc-unix-roundtrip"),
        env = "SERVICE_DAEMON_RS_LOCAL_IPC_UNIX_ROUNDTRIP_NAME_0CFD61B9"
    )]
    pub struct UnixRoundtripServer;

    #[derive(Debug)]
    #[provider(
        LocalIpcConnect("service-daemon-rs-local-ipc-unix-roundtrip"),
        env = "SERVICE_DAEMON_RS_LOCAL_IPC_UNIX_ROUNDTRIP_NAME_0CFD61B9"
    )]
    pub struct UnixRoundtripClient;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unix_local_ipc_listen_connect_roundtrip() {
        let _env_lock = ENV_VAR_LOCK.lock().await;
        let (_env_var, logical_name) = set_local_ipc_name(ROUNDTRIP_ENV_VAR, "unix-roundtrip");

        let server = <UnixRoundtripServer as ManagedProvided>::resolve_managed()
            .await
            .expect("UnixRoundtripServer resolve failed");
        assert_eq!(server.name(), logical_name);

        let server_task = tokio::spawn(async move {
            let mut stream = server
                .accept()
                .await
                .expect("Failed to accept the real roundtrip connection");

            let mut buf = [0_u8; REQUEST_PAYLOAD.len()];
            stream
                .read_exact(&mut buf)
                .await
                .expect("Server read_exact failed");
            stream
                .write_all(RESPONSE_PAYLOAD)
                .await
                .expect("Server write_all failed");
            buf
        });

        let client = <UnixRoundtripClient as ManagedProvided>::resolve_managed()
            .await
            .expect("UnixRoundtripClient resolve failed");
        assert_eq!(client.name(), logical_name);

        let mut conn = client
            .connect()
            .await
            .expect("UnixRoundtripClient.connect failed");
        conn.write_all(REQUEST_PAYLOAD)
            .await
            .expect("Client write_all failed");

        let mut response = [0_u8; RESPONSE_PAYLOAD.len()];
        conn.read_exact(&mut response)
            .await
            .expect("Client read_exact failed");

        assert_eq!(&response, RESPONSE_PAYLOAD);

        let received = server_task.await.expect("Server task panicked");
        assert_eq!(&received, REQUEST_PAYLOAD);
    }

    #[derive(Debug)]
    #[provider(
        LocalIpcListen("service-daemon-rs-local-ipc-unix-unused-server"),
        env = "SERVICE_DAEMON_RS_LOCAL_IPC_UNIX_OVERRIDE_NAME_A49E8522"
    )]
    pub struct UnixOverrideServer;

    #[derive(Debug)]
    #[provider(
        LocalIpcConnect("service-daemon-rs-local-ipc-unix-unused-client"),
        env = "SERVICE_DAEMON_RS_LOCAL_IPC_UNIX_OVERRIDE_NAME_A49E8522"
    )]
    pub struct UnixOverrideClient;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unix_local_ipc_env_overrides_logical_name() {
        let _env_lock = ENV_VAR_LOCK.lock().await;
        let (_env_var, logical_name) = set_local_ipc_name(OVERRIDE_ENV_VAR, "unix-override");

        let server = <UnixOverrideServer as ManagedProvided>::resolve_managed()
            .await
            .expect("UnixOverrideServer resolve failed");
        assert_eq!(server.name(), logical_name);

        let server_task = tokio::spawn(async move {
            let mut stream = server
                .accept()
                .await
                .expect("Failed to accept the env override business connection");
            stream
                .write_all(RESPONSE_PAYLOAD)
                .await
                .expect("Server write_all failed");
        });

        let client = <UnixOverrideClient as ManagedProvided>::resolve_managed()
            .await
            .expect("UnixOverrideClient resolve failed");
        assert_eq!(client.name(), logical_name);

        let mut conn = client
            .connect()
            .await
            .expect("UnixOverrideClient.connect failed");
        let mut response = [0_u8; RESPONSE_PAYLOAD.len()];
        conn.read_exact(&mut response)
            .await
            .expect("Client read_exact failed");
        assert_eq!(&response, RESPONSE_PAYLOAD);

        server_task.await.expect("Server task panicked");
    }

    #[derive(Debug)]
    #[provider(
        LocalIpcListen("service-daemon-rs-local-ipc-unix-valid-fallback"),
        env = "SERVICE_DAEMON_RS_LOCAL_IPC_UNIX_INVALID_LISTEN_NAME_C64173F7"
    )]
    pub struct UnixInvalidEnvServer;

    #[derive(Debug)]
    #[provider(
        LocalIpcConnect("service-daemon-rs-local-ipc-unix-valid-fallback"),
        env = "SERVICE_DAEMON_RS_LOCAL_IPC_UNIX_INVALID_CONNECT_NAME_F23F5C14"
    )]
    pub struct UnixInvalidEnvClient;

    #[tokio::test]
    async fn unix_local_ipc_listen_env_override_invalid_logical_name_is_fatal() {
        let _env_lock = ENV_VAR_LOCK.lock().await;
        let _env_var = set_raw_env_value(INVALID_LISTEN_ENV_VAR, "bad/name");

        match <UnixInvalidEnvServer as ManagedProvided>::resolve_managed().await {
            Err(ProviderError::Fatal(message)) => {
                assert!(message.contains("logical name"), "{message}");
                assert!(message.contains("bad/name"), "{message}");
            }
            other => panic!("expected fatal invalid LocalIpc env name error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unix_local_ipc_connect_env_override_invalid_logical_name_is_fatal() {
        let _env_lock = ENV_VAR_LOCK.lock().await;
        let _env_var = set_raw_env_value(INVALID_CONNECT_ENV_VAR, "bad/name");

        match <UnixInvalidEnvClient as ManagedProvided>::resolve_managed().await {
            Err(ProviderError::Fatal(message)) => {
                assert!(message.contains("logical name"), "{message}");
                assert!(message.contains("bad/name"), "{message}");
            }
            other => panic!("expected fatal invalid LocalIpc env name error, got {other:?}"),
        }
    }
}

#[cfg(windows)]
mod windows_tests {
    use super::*;

    const ROUNDTRIP_ENV_VAR: &str = "SERVICE_DAEMON_RS_LOCAL_IPC_WINDOWS_ROUNDTRIP_NAME_0CFD61B9";
    const OVERRIDE_ENV_VAR: &str = "SERVICE_DAEMON_RS_LOCAL_IPC_WINDOWS_OVERRIDE_NAME_A49E8522";
    const INVALID_LISTEN_ENV_VAR: &str =
        "SERVICE_DAEMON_RS_LOCAL_IPC_WINDOWS_INVALID_LISTEN_NAME_C64173F7";
    const INVALID_CONNECT_ENV_VAR: &str =
        "SERVICE_DAEMON_RS_LOCAL_IPC_WINDOWS_INVALID_CONNECT_NAME_F23F5C14";
    const BUSY_RETRY_ENV_VAR: &str = "SERVICE_DAEMON_RS_LOCAL_IPC_WINDOWS_BUSY_RETRY_NAME_E2844B06";

    fn pipe_name(logical_name: &str) -> String {
        format!(r"\\.\pipe\service-daemon-rs-{logical_name}")
    }

    fn create_single_instance_server(
        pipe_name: &str,
    ) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
        let mut options = tokio::net::windows::named_pipe::ServerOptions::new();
        options.max_instances(1);
        options.create(pipe_name)
    }

    #[derive(Debug)]
    #[provider(
        LocalIpcListen("service-daemon-rs-local-ipc-windows-roundtrip"),
        env = "SERVICE_DAEMON_RS_LOCAL_IPC_WINDOWS_ROUNDTRIP_NAME_0CFD61B9"
    )]
    pub struct WindowsRoundtripServer;

    #[derive(Debug)]
    #[provider(
        LocalIpcConnect("service-daemon-rs-local-ipc-windows-roundtrip"),
        env = "SERVICE_DAEMON_RS_LOCAL_IPC_WINDOWS_ROUNDTRIP_NAME_0CFD61B9"
    )]
    pub struct WindowsRoundtripClient;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn windows_local_ipc_listen_connect_roundtrip() {
        let _env_lock = ENV_VAR_LOCK.lock().await;
        let (_env_var, logical_name) = set_local_ipc_name(ROUNDTRIP_ENV_VAR, "windows-roundtrip");

        let server = <WindowsRoundtripServer as ManagedProvided>::resolve_managed()
            .await
            .expect("WindowsRoundtripServer resolve failed");
        assert_eq!(server.name(), logical_name);

        let server_task = tokio::spawn(async move {
            let mut stream = server
                .accept()
                .await
                .expect("Failed to accept the real roundtrip connection");

            let mut buf = [0_u8; REQUEST_PAYLOAD.len()];
            stream
                .read_exact(&mut buf)
                .await
                .expect("Server read_exact failed");
            stream
                .write_all(RESPONSE_PAYLOAD)
                .await
                .expect("Server write_all failed");
            buf
        });

        let client = <WindowsRoundtripClient as ManagedProvided>::resolve_managed()
            .await
            .expect("WindowsRoundtripClient resolve failed");
        assert_eq!(client.name(), logical_name);

        let mut conn = client
            .connect()
            .await
            .expect("WindowsRoundtripClient.connect failed");
        conn.write_all(REQUEST_PAYLOAD)
            .await
            .expect("Client write_all failed");

        let mut response = [0_u8; RESPONSE_PAYLOAD.len()];
        conn.read_exact(&mut response)
            .await
            .expect("Client read_exact failed");

        assert_eq!(&response, RESPONSE_PAYLOAD);

        let received = server_task.await.expect("Server task panicked");
        assert_eq!(&received, REQUEST_PAYLOAD);
    }

    #[derive(Debug)]
    #[provider(
        LocalIpcListen("service-daemon-rs-local-ipc-windows-unused-server"),
        env = "SERVICE_DAEMON_RS_LOCAL_IPC_WINDOWS_OVERRIDE_NAME_A49E8522"
    )]
    pub struct WindowsOverrideServer;

    #[derive(Debug)]
    #[provider(
        LocalIpcConnect("service-daemon-rs-local-ipc-windows-unused-client"),
        env = "SERVICE_DAEMON_RS_LOCAL_IPC_WINDOWS_OVERRIDE_NAME_A49E8522"
    )]
    pub struct WindowsOverrideClient;

    #[derive(Debug)]
    #[provider(
        LocalIpcConnect("service-daemon-rs-local-ipc-windows-busy-retry"),
        env = "SERVICE_DAEMON_RS_LOCAL_IPC_WINDOWS_BUSY_RETRY_NAME_E2844B06"
    )]
    pub struct WindowsBusyRetryClient;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn windows_local_ipc_env_overrides_logical_name() {
        let _env_lock = ENV_VAR_LOCK.lock().await;
        let (_env_var, logical_name) = set_local_ipc_name(OVERRIDE_ENV_VAR, "windows-override");

        let server = <WindowsOverrideServer as ManagedProvided>::resolve_managed()
            .await
            .expect("WindowsOverrideServer resolve failed");
        assert_eq!(server.name(), logical_name);

        let server_task = tokio::spawn(async move {
            let mut stream = server
                .accept()
                .await
                .expect("Failed to accept the env override business connection");
            stream
                .write_all(RESPONSE_PAYLOAD)
                .await
                .expect("Server write_all failed");
        });

        let client = <WindowsOverrideClient as ManagedProvided>::resolve_managed()
            .await
            .expect("WindowsOverrideClient resolve failed");
        assert_eq!(client.name(), logical_name);

        let mut conn = client
            .connect()
            .await
            .expect("WindowsOverrideClient.connect failed");
        let mut response = [0_u8; RESPONSE_PAYLOAD.len()];
        conn.read_exact(&mut response)
            .await
            .expect("Client read_exact failed");
        assert_eq!(&response, RESPONSE_PAYLOAD);

        server_task.await.expect("Server task panicked");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn windows_local_ipc_connect_retries_busy_pipe_replacement_gap() {
        let _env_lock = ENV_VAR_LOCK.lock().await;
        let (_env_var, logical_name) = set_local_ipc_name(BUSY_RETRY_ENV_VAR, "windows-busy-retry");
        let pipe_name = pipe_name(&logical_name);

        let busy_server =
            create_single_instance_server(&pipe_name).expect("busy server create failed");
        let busy_client = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&pipe_name)
            .expect("busy holder client open failed");

        let provider = <WindowsBusyRetryClient as ManagedProvided>::resolve_managed()
            .await
            .expect("WindowsBusyRetryClient resolve failed");
        assert_eq!(provider.name(), logical_name);

        let release_name = pipe_name.clone();
        let release_task = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            drop(busy_client);
            drop(busy_server);
            let replacement = create_single_instance_server(&release_name)
                .expect("replacement server create failed");
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            drop(replacement);
        });

        let result = provider.connect().await;
        assert!(
            result.is_ok(),
            "expected LocalIpcConnect busy retry to survive replacement gap, got {result:?}"
        );
        release_task.abort();
    }

    #[derive(Debug)]
    #[provider(
        LocalIpcListen("service-daemon-rs-local-ipc-windows-valid-fallback"),
        env = "SERVICE_DAEMON_RS_LOCAL_IPC_WINDOWS_INVALID_LISTEN_NAME_C64173F7"
    )]
    pub struct WindowsInvalidEnvServer;

    #[derive(Debug)]
    #[provider(
        LocalIpcConnect("service-daemon-rs-local-ipc-windows-valid-fallback"),
        env = "SERVICE_DAEMON_RS_LOCAL_IPC_WINDOWS_INVALID_CONNECT_NAME_F23F5C14"
    )]
    pub struct WindowsInvalidEnvClient;

    #[tokio::test]
    async fn windows_local_ipc_listen_env_override_invalid_logical_name_is_fatal() {
        let _env_lock = ENV_VAR_LOCK.lock().await;
        let _env_var = set_raw_env_value(INVALID_LISTEN_ENV_VAR, "bad/name");

        match <WindowsInvalidEnvServer as ManagedProvided>::resolve_managed().await {
            Err(ProviderError::Fatal(message)) => {
                assert!(message.contains("logical name"), "{message}");
                assert!(message.contains("bad/name"), "{message}");
            }
            other => panic!("expected fatal invalid LocalIpc env name error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn windows_local_ipc_connect_env_override_invalid_logical_name_is_fatal() {
        let _env_lock = ENV_VAR_LOCK.lock().await;
        let _env_var = set_raw_env_value(INVALID_CONNECT_ENV_VAR, "bad/name");

        match <WindowsInvalidEnvClient as ManagedProvided>::resolve_managed().await {
            Err(ProviderError::Fatal(message)) => {
                assert!(message.contains("logical name"), "{message}");
                assert!(message.contains("bad/name"), "{message}");
            }
            other => panic!("expected fatal invalid LocalIpc env name error, got {other:?}"),
        }
    }
}
