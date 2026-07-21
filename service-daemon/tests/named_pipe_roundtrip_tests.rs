// End-to-end test: NamedPipeListen and NamedPipeConnect cooperating on the
// same Windows local named pipe.
//
// This mirrors the Unix domain socket roundtrip shape: the first connection is
// the client provider's init-time probe, and the second connection carries the
// application exchange.

#![cfg(windows)]

use service_daemon::{ManagedProvided, provider};
use std::ffi::OsString;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

static ENV_VAR_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
const ROUNDTRIP_NAME_ENV: &str = "SERVICE_DAEMON_RS_NAMED_PIPE_ROUNDTRIP_NAME_B2301705";

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        let _lock = ENV_VAR_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        unsafe {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

fn set_test_env(key: &'static str, value: &str) -> EnvVarGuard {
    let _lock = ENV_VAR_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous = std::env::var_os(key);
    unsafe {
        std::env::set_var(key, value);
    }
    EnvVarGuard { key, previous }
}

fn unique_pipe_name(name: &str) -> String {
    format!(
        r"\\.\pipe\service-daemon-rs-{name}-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    )
}

#[derive(Debug)]
#[provider(
    NamedPipeListen(r"\\.\pipe\service-daemon-rs-roundtrip-fallback"),
    env = "SERVICE_DAEMON_RS_NAMED_PIPE_ROUNDTRIP_NAME_B2301705"
)]
pub struct RoundtripServer;

#[derive(Debug)]
#[provider(
    NamedPipeConnect(r"\\.\pipe\service-daemon-rs-roundtrip-fallback"),
    env = "SERVICE_DAEMON_RS_NAMED_PIPE_ROUNDTRIP_NAME_B2301705"
)]
pub struct RoundtripClient;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn named_pipe_listen_connect_roundtrip() {
    let name = unique_pipe_name("roundtrip");
    let _env = set_test_env(ROUNDTRIP_NAME_ENV, &name);

    let server = <RoundtripServer as ManagedProvided>::resolve_managed()
        .await
        .expect("RoundtripServer resolve failed");

    let probe_accepted = Arc::new(tokio::sync::Notify::new());
    let probe_accepted_task = Arc::clone(&probe_accepted);

    let server_task = tokio::spawn(async move {
        let probe = server
            .accept()
            .await
            .expect("Failed to accept the init-time probe connection");
        drop(probe);
        probe_accepted_task.notify_one();

        let mut pipe = server
            .accept()
            .await
            .expect("Failed to accept the real roundtrip connection");

        let mut buf = [0u8; 5];
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
    tokio::time::timeout(Duration::from_secs(5), probe_accepted.notified())
        .await
        .expect("server did not accept the init-time probe");

    let mut pipe = client
        .connect()
        .await
        .expect("RoundtripClient.connect failed");
    pipe.write_all(b"hello")
        .await
        .expect("Client write_all failed");

    let mut response = [0u8; 5];
    pipe.read_exact(&mut response)
        .await
        .expect("Client read_exact failed");

    assert_eq!(&response, b"world", "client should receive 'world'");

    let received = server_task.await.expect("Server task panicked");
    assert_eq!(&received, b"hello", "server should receive 'hello'");
}
