// End-to-end test: NamedPipeListen and NamedPipeConnect cooperating on one
// Windows named pipe.
//
// `NamedPipeConnect` resolves as a lightweight endpoint handle. The server-side
// accept loop expects only the real roundtrip stream.

#![cfg(windows)]

use service_daemon::{ManagedProvided, provider};
use std::ffi::OsString;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ROUNDTRIP_ENV_VAR: &str = "SERVICE_DAEMON_RS_NAMED_PIPE_ROUNDTRIP_NAME_B1741D2A";

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(0);

struct EnvVarGuard {
    previous: Option<OsString>,
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // Rust 2024 marks environment mutation unsafe because it is process-global.
        unsafe {
            if let Some(previous) = &self.previous {
                std::env::set_var(ROUNDTRIP_ENV_VAR, previous);
            } else {
                std::env::remove_var(ROUNDTRIP_ENV_VAR);
            }
        }
    }
}

fn set_roundtrip_pipe_name() -> EnvVarGuard {
    let counter = PIPE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pipe_name = format!(
        r"\\.\pipe\service-daemon-rs-roundtrip-provider-{}-{counter}",
        std::process::id(),
    );
    let previous = std::env::var_os(ROUNDTRIP_ENV_VAR);
    // Rust 2024 marks environment mutation unsafe because it is process-global.
    unsafe {
        std::env::set_var(ROUNDTRIP_ENV_VAR, pipe_name);
    }
    EnvVarGuard { previous }
}

#[derive(Debug)]
#[provider(
    NamedPipeListen(r"\\.\pipe\service-daemon-rs-roundtrip-provider"),
    env = "SERVICE_DAEMON_RS_NAMED_PIPE_ROUNDTRIP_NAME_B1741D2A"
)]
pub struct RoundtripServer;

#[derive(Debug)]
#[provider(
    NamedPipeConnect(r"\\.\pipe\service-daemon-rs-roundtrip-provider"),
    env = "SERVICE_DAEMON_RS_NAMED_PIPE_ROUNDTRIP_NAME_B1741D2A"
)]
pub struct RoundtripClient;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_named_pipe_listen_connect_roundtrip() {
    let _env_var = set_roundtrip_pipe_name();

    let server = <RoundtripServer as ManagedProvided>::resolve_managed()
        .await
        .expect("RoundtripServer resolve failed");

    let server_task = tokio::spawn(async move {
        let mut pipe = server
            .accept()
            .await
            .expect("Failed to accept the real roundtrip connection");

        let mut buf = [0_u8; 5];
        pipe.read_exact(&mut buf)
            .await
            .expect("Server read_exact failed");
        pipe.write_all(b"world")
            .await
            .expect("Server write_all failed");
        buf
    });

    let client = <RoundtripClient as ManagedProvided>::resolve_managed()
        .await
        .expect("RoundtripClient resolve failed");

    let mut conn = client
        .connect()
        .await
        .expect("RoundtripClient.connect failed");
    conn.write_all(b"hello")
        .await
        .expect("Client write_all failed");

    let mut response = [0_u8; 5];
    conn.read_exact(&mut response)
        .await
        .expect("Client read_exact failed");

    assert_eq!(&response, b"world", "client should receive 'world'");

    let received = server_task.await.expect("Server task panicked");
    assert_eq!(&received, b"hello", "server should receive 'hello'");
}
