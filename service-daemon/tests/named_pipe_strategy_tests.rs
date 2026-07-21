// Integration tests for the `#[provider(NamedPipeListen(...))]` and
// `#[provider(NamedPipeConnect(...))]` templates.
//
// Windows named pipes are Windows-only Tokio APIs, so this file is excluded
// entirely on non-Windows targets. Linux/macOS still cover parser and
// non-Windows diagnostics through trybuild.

#![cfg(windows)]

use service_daemon::{ManagedProvided, RestartPolicy, ServiceDaemon, provider, service};
use std::ffi::OsString;
use std::path::Path;
use std::sync::{
    LazyLock, Mutex, MutexGuard,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Duration;
use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};

const RETRY_ENV_VAR: &str = "SERVICE_DAEMON_RS_NAMED_PIPE_RETRY_NAME_4F5DF1E3";
const BUSY_ENV_VAR: &str = "SERVICE_DAEMON_RS_NAMED_PIPE_BUSY_NAME_B6B38F16";
const MISSING_ENV_VAR: &str = "SERVICE_DAEMON_RS_NAMED_PIPE_MISSING_NAME_29AA2D83";
const ERROR_PIPE_BUSY: i32 = 231;

static RETRY_SERVICE_ENTERED: AtomicBool = AtomicBool::new(false);
static BUSY_SERVICE_ENTERED: AtomicBool = AtomicBool::new(false);
static MISSING_SERVICE_ENTERED: AtomicBool = AtomicBool::new(false);
static RETRY_SERVICE_READY: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);
static BUSY_SERVICE_READY: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);
static ENV_VAR_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
static PIPE_COUNTER: AtomicU64 = AtomicU64::new(0);

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

fn unique_pipe_name(label: &str) -> String {
    let counter = PIPE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        r"\\.\pipe\service-daemon-rs-{label}-{}-{counter}",
        std::process::id(),
    )
}

fn create_server(
    pipe_name: &str,
) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    ServerOptions::new()
        .reject_remote_clients(true)
        .create(pipe_name)
}

// ---------------------------------------------------------------------------
// Listen provider initializes and exposes its configured pipe name.
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[provider(NamedPipeListen(r"\\.\pipe\service-daemon-rs-listen-fresh"))]
pub struct FreshPipeListener;

#[tokio::test]
async fn test_named_pipe_listen_fresh_name_ok() {
    let result = <FreshPipeListener as ManagedProvided>::resolve_managed().await;
    let provider = result.expect("Expected fresh named pipe listener to initialize");
    assert_eq!(
        provider.name(),
        Path::new(r"\\.\pipe\service-daemon-rs-listen-fresh"),
        "name() should return the configured pipe name"
    );
}

// ---------------------------------------------------------------------------
// Connect succeeds when a peer server instance is already available.
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[provider(
    NamedPipeConnect(r"\\.\pipe\service-daemon-rs-connect-ready-fallback"),
    env = "SERVICE_DAEMON_RS_NAMED_PIPE_RETRY_NAME_4F5DF1E3"
)]
pub struct ReadyPipeClient;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_named_pipe_connect_succeeds_when_server_ready() -> anyhow::Result<()> {
    let pipe_name = unique_pipe_name("connect-ready");
    let _env_var = set_test_env(RETRY_ENV_VAR, &pipe_name);

    let server = create_server(&pipe_name)?;
    let server_task = tokio::spawn(async move { server.connect().await });

    let result = <ReadyPipeClient as ManagedProvided>::resolve_managed().await;
    assert!(
        result.is_ok(),
        "Expected connect provider to succeed when peer server exists, got {:?}",
        result
    );

    server_task.await??;
    Ok(())
}

// ---------------------------------------------------------------------------
// Missing peer is retryable until provider-init timeout stops startup.
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[provider(
    NamedPipeConnect(r"\\.\pipe\service-daemon-rs-missing-fallback"),
    env = "SERVICE_DAEMON_RS_NAMED_PIPE_MISSING_NAME_29AA2D83",
    eager = true
)]
pub struct MissingPipeClient;

#[service(tags = ["named_pipe_missing_peer_provider_test"])]
async fn missing_pipe_client_service(
    _client: std::sync::Arc<MissingPipeClient>,
) -> anyhow::Result<()> {
    MISSING_SERVICE_ENTERED.store(true, Ordering::SeqCst);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_named_pipe_connect_missing_peer_returns_provider_init_error() {
    let pipe_name = unique_pipe_name("missing-peer");
    let _env_var = set_test_env(MISSING_ENV_VAR, &pipe_name);
    MISSING_SERVICE_ENTERED.store(false, Ordering::SeqCst);

    let mut daemon = ServiceDaemon::builder()
        .with_registry(
            service_daemon::Registry::builder()
                .with_tag("named_pipe_missing_peer_provider_test")
                .build(),
        )
        .with_restart_policy(
            RestartPolicy::builder()
                .initial_delay(Duration::from_millis(1))
                .max_delay(Duration::from_millis(5))
                .jitter_factor(0.0)
                .provider_init_timeout(Duration::from_millis(20))
                .build(),
        )
        .build();

    daemon.run().await;

    assert!(daemon.cancel_token().is_cancelled());
    assert!(!MISSING_SERVICE_ENTERED.load(Ordering::SeqCst));
}

// ---------------------------------------------------------------------------
// Connect retries through NotFound until the peer server appears.
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[provider(
    NamedPipeConnect(r"\\.\pipe\service-daemon-rs-retry-fallback"),
    env = "SERVICE_DAEMON_RS_NAMED_PIPE_RETRY_NAME_4F5DF1E3",
    eager = true
)]
pub struct RetryPipeClient;

#[service(tags = ["named_pipe_retry_provider_test"])]
async fn retry_pipe_client_service(client: std::sync::Arc<RetryPipeClient>) -> anyhow::Result<()> {
    RETRY_SERVICE_ENTERED.store(
        client.name() == Path::new(&std::env::var(RETRY_ENV_VAR)?),
        Ordering::SeqCst,
    );
    RETRY_SERVICE_READY.notify_one();
    service_daemon::done();
    service_daemon::wait_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_named_pipe_connect_retries_until_peer_appears() -> anyhow::Result<()> {
    let pipe_name = unique_pipe_name("retry-peer");
    let _env_var = set_test_env(RETRY_ENV_VAR, &pipe_name);
    RETRY_SERVICE_ENTERED.store(false, Ordering::SeqCst);

    let delayed_pipe = pipe_name.clone();
    let server_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let server = create_server(&delayed_pipe)?;
        server.connect().await
    });

    let mut daemon = ServiceDaemon::builder()
        .with_registry(
            service_daemon::Registry::builder()
                .with_tag("named_pipe_retry_provider_test")
                .build(),
        )
        .with_restart_policy(
            RestartPolicy::builder()
                .initial_delay(Duration::from_millis(1))
                .max_delay(Duration::from_millis(5))
                .jitter_factor(0.0)
                .provider_init_timeout(Duration::from_secs(1))
                .build(),
        )
        .build();
    let cancel = daemon.cancel_token();

    daemon.run().await;
    if !RETRY_SERVICE_ENTERED.load(Ordering::SeqCst) {
        tokio::time::timeout(Duration::from_secs(5), RETRY_SERVICE_READY.notified()).await?;
    }

    assert!(RETRY_SERVICE_ENTERED.load(Ordering::SeqCst));
    server_task.await??;
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), daemon.wait()).await??;
    Ok(())
}

// ---------------------------------------------------------------------------
// Connect retries raw ERROR_PIPE_BUSY until a server instance is available.
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[provider(
    NamedPipeConnect(r"\\.\pipe\service-daemon-rs-busy-fallback"),
    env = "SERVICE_DAEMON_RS_NAMED_PIPE_BUSY_NAME_B6B38F16",
    eager = true
)]
pub struct BusyPipeClient;

#[service(tags = ["named_pipe_busy_provider_test"])]
async fn busy_pipe_client_service(client: std::sync::Arc<BusyPipeClient>) -> anyhow::Result<()> {
    BUSY_SERVICE_ENTERED.store(
        client.name() == Path::new(&std::env::var(BUSY_ENV_VAR)?),
        Ordering::SeqCst,
    );
    BUSY_SERVICE_READY.notify_one();
    service_daemon::done();
    service_daemon::wait_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_named_pipe_connect_retries_error_pipe_busy() -> anyhow::Result<()> {
    let pipe_name = unique_pipe_name("busy-peer");
    let _env_var = set_test_env(BUSY_ENV_VAR, &pipe_name);
    BUSY_SERVICE_ENTERED.store(false, Ordering::SeqCst);

    let mut busy_options = ServerOptions::new();
    busy_options.reject_remote_clients(true).max_instances(1);
    let busy_server = busy_options.create(&pipe_name)?;
    let busy_client = ClientOptions::new().open(&pipe_name)?;
    busy_server.connect().await?;

    let busy_check = ClientOptions::new().open(&pipe_name).unwrap_err();
    assert_eq!(
        busy_check.raw_os_error(),
        Some(ERROR_PIPE_BUSY),
        "test precondition failed: second client should see ERROR_PIPE_BUSY"
    );

    let delayed_pipe = pipe_name.clone();
    let release_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(busy_client);
        drop(busy_server);
        let server = create_server(&delayed_pipe)?;
        server.connect().await
    });

    let mut daemon = ServiceDaemon::builder()
        .with_registry(
            service_daemon::Registry::builder()
                .with_tag("named_pipe_busy_provider_test")
                .build(),
        )
        .with_restart_policy(
            RestartPolicy::builder()
                .initial_delay(Duration::from_millis(1))
                .max_delay(Duration::from_millis(5))
                .jitter_factor(0.0)
                .provider_init_timeout(Duration::from_secs(1))
                .build(),
        )
        .build();
    let cancel = daemon.cancel_token();

    daemon.run().await;
    if !BUSY_SERVICE_ENTERED.load(Ordering::SeqCst) {
        tokio::time::timeout(Duration::from_secs(5), BUSY_SERVICE_READY.notified()).await?;
    }

    assert!(BUSY_SERVICE_ENTERED.load(Ordering::SeqCst));
    release_task.await??;
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), daemon.wait()).await??;
    Ok(())
}
