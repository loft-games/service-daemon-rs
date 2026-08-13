// Integration tests for the `#[provider(UnixConnect("..."))]` template.
//
// `UnixConnect` is a lightweight endpoint handle. Resolving the provider parses
// the configured path only; dialing happens when user code calls `connect()` or
// the lower-level `try_connect()`.

#![cfg(unix)]

use service_daemon::{ManagedProvided, provider};
use std::ffi::OsString;
use std::sync::{LazyLock, Mutex, MutexGuard};

static ENV_VAR_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

const ENV_OVERRIDE_ENV_VAR: &str = "SERVICE_DAEMON_RS_UNIX_CONNECT_ENV_OVERRIDE_PATH_8E16B4A9";
const ENV_OVERRIDE_PATH: &str = "target/sd-uds-connect-env-override-env.sock";

fn cleanup_path(path: &str) {
    match std::fs::remove_file(path) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("cleanup_path({}) failed: {} (ignored in test)", path, e),
    }
}

struct PathGuard(&'static str);
impl Drop for PathGuard {
    fn drop(&mut self) {
        cleanup_path(self.0);
    }
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
    _lock: MutexGuard<'static, ()>,
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

fn set_test_env(key: &'static str, value: &str) -> EnvVarGuard {
    let lock = ENV_VAR_LOCK.lock().expect("env var test lock poisoned");
    let previous = std::env::var_os(key);
    // Rust 2024 marks environment mutation unsafe because it is process-global.
    unsafe {
        std::env::set_var(key, value);
    }
    EnvVarGuard {
        key,
        previous,
        _lock: lock,
    }
}

fn prepare_socket_path(path: &'static str) -> PathGuard {
    if let Some(parent) = std::path::Path::new(path).parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).expect("Failed to create socket test directory");
    }
    cleanup_path(path);
    PathGuard(path)
}

#[derive(Debug)]
#[provider(UnixConnect("target/sd-uds-connect-resolve-missing.sock"))]
pub struct MissingAtResolveClient;

#[tokio::test]
async fn test_unix_connect_resolves_without_peer() {
    let path = "target/sd-uds-connect-resolve-missing.sock";
    let _guard = prepare_socket_path(path);

    let provider = <MissingAtResolveClient as ManagedProvided>::resolve_managed()
        .await
        .expect("UnixConnect provider should resolve without dialing the peer");
    assert_eq!(provider.path(), std::path::Path::new(path));
}

#[derive(Debug)]
#[provider(UnixConnect("target/sd-uds-connect-call-missing.sock"))]
pub struct MissingAtConnectClient;

#[tokio::test]
async fn test_unix_connect_missing_peer_errors_on_connect_call() {
    let path = "target/sd-uds-connect-call-missing.sock";
    let _guard = prepare_socket_path(path);

    let provider = <MissingAtConnectClient as ManagedProvided>::resolve_managed()
        .await
        .expect("UnixConnect provider should resolve without dialing the peer");

    let error = provider
        .connect()
        .await
        .expect_err("connect() should report the missing peer at the call site");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}

#[derive(Debug)]
#[provider(
    UnixConnect("target/sd-uds-connect-env-override-fallback.sock"),
    env = "SERVICE_DAEMON_RS_UNIX_CONNECT_ENV_OVERRIDE_PATH_8E16B4A9",
    eager = true
)]
pub struct EnvOverrideClient;

#[tokio::test]
async fn test_unix_connect_env_overrides_fallback_without_peer_probe() {
    let _env_var = set_test_env(ENV_OVERRIDE_ENV_VAR, ENV_OVERRIDE_PATH);
    let _guard = prepare_socket_path(ENV_OVERRIDE_PATH);

    let provider = <EnvOverrideClient as ManagedProvided>::resolve_managed()
        .await
        .expect("env override should resolve without requiring a listening peer");

    assert_eq!(provider.path(), std::path::Path::new(ENV_OVERRIDE_PATH));
}

#[derive(Debug)]
#[provider(UnixConnect("target/sd-uds-connect-indep.sock"))]
pub struct IndepClient;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_unix_connect_provides_independent_raw_streams_per_call() {
    let path = "target/sd-uds-connect-indep.sock";
    let _guard = prepare_socket_path(path);

    let _peer = std::os::unix::net::UnixListener::bind(path).expect("peer bind failed");

    let provider = <IndepClient as ManagedProvided>::resolve_managed()
        .await
        .expect("resolve_managed failed for IndepClient");

    let s1 = provider
        .try_connect()
        .await
        .expect("first try_connect failed");
    let s2 = provider
        .try_connect()
        .await
        .expect("second try_connect failed");

    drop(s2);
    drop(s1);
}

#[derive(Debug)]
#[provider(UnixConnect("target/sd-uds-connect-convenience.sock"))]
pub struct ConvenienceClient;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_unix_connect_convenience_method_opens_ipc_streams() {
    let path = "target/sd-uds-connect-convenience.sock";
    let _guard = prepare_socket_path(path);

    let _peer = std::os::unix::net::UnixListener::bind(path).expect("peer bind failed");

    let provider = <ConvenienceClient as ManagedProvided>::resolve_managed()
        .await
        .expect("resolve_managed failed for ConvenienceClient");

    let first_stream = provider.connect().await.expect("first connect failed");
    let second_stream = provider.connect().await.expect("second connect failed");

    drop(second_stream);
    drop(first_stream);
}
