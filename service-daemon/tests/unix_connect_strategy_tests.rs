// Integration tests for the `#[provider(UnixConnect("..."))]` template.
//
// See `unix_listen_strategy_tests.rs` for the rationale on per-test struct +
// path layout, RAII path guards, and Windows-handoff caveats. The same
// constraints apply here.

#![cfg(unix)]

use service_daemon::{ManagedProvided, RestartPolicy, ServiceDaemon, provider, service};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{
    LazyLock, Mutex, MutexGuard,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

static MISSING_PEER_SERVICE_ENTERED: AtomicBool = AtomicBool::new(false);
static ENV_EAGER_SERVICE_ENTERED: AtomicBool = AtomicBool::new(false);
static ENV_EAGER_SERVICE_SAW_ENV_PATH: AtomicBool = AtomicBool::new(false);
static ENV_EAGER_SERVICE_SAW_PROBE: AtomicBool = AtomicBool::new(false);
static ENV_EAGER_PROBE_ACCEPTED: AtomicBool = AtomicBool::new(false);
static ENV_EAGER_SERVICE_READY: LazyLock<tokio::sync::Notify> =
    LazyLock::new(tokio::sync::Notify::new);
static ENV_EAGER_PROBE_READY: LazyLock<tokio::sync::Notify> =
    LazyLock::new(tokio::sync::Notify::new);
static ENV_VAR_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

const ENV_EAGER_ENV_VAR: &str = "SERVICE_DAEMON_RS_UNIX_CONNECT_ENV_EAGER_PATH_8E16B4A9";
const ENV_EAGER_PATH: &str = "target/sd-uds-connect-env-eager-env.sock";
const ENV_EAGER_FALLBACK_PATH: &str = "target/sd-uds-connect-env-eager-fallback.sock";
const RETRY_ENV_VAR: &str = "SERVICE_DAEMON_RS_UNIX_CONNECT_RETRY_PATH_4D39F2C1";

fn cleanup_path(path: &str) {
    match std::fs::remove_file(path) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("cleanup_path({}) failed: {} (ignored in test)", path, e),
    }
}

fn cleanup_pathbuf(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!(
            "cleanup_path({}) failed: {} (ignored in test)",
            path.display(),
            e
        ),
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

struct OwnedPathGuard(PathBuf);
impl Drop for OwnedPathGuard {
    fn drop(&mut self) {
        cleanup_pathbuf(&self.0);
    }
}

fn unique_socket_path(name: &str) -> PathBuf {
    PathBuf::from("target").join(format!(
        "service-daemon-rs-{name}-{}-{}.sock",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ))
}

fn prepare_owned_socket_path(path: PathBuf) -> OwnedPathGuard {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).expect("Failed to create socket test directory");
    }
    cleanup_pathbuf(&path);
    OwnedPathGuard(path)
}

// ---------------------------------------------------------------------------
// Connect succeeds when the peer is already listening at init time.
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[provider(UnixConnect("target/sd-uds-connect-ok.sock"))]
pub struct OkClient;

#[tokio::test]
async fn test_unix_connect_succeeds_when_server_ready() {
    let path = "target/sd-uds-connect-ok.sock";
    let _guard = prepare_socket_path(path);

    // Bring up a peer listener BEFORE the provider resolves. This satisfies
    // the init-time probe on the first attempt without exercising retry/backoff.
    let _peer =
        std::os::unix::net::UnixListener::bind(path).expect("Failed to bring up the peer listener");

    let result = <OkClient as ManagedProvided>::resolve_managed().await;
    assert!(
        result.is_ok(),
        "Expected immediate-success connect, got {:?}",
        result
    );
}

// ---------------------------------------------------------------------------
// Connect retries through ConnectionRefused/NotFound until the peer shows up.
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[provider(
    UnixConnect("target/sd-uds-connect-retry.sock"),
    env = "SERVICE_DAEMON_RS_UNIX_CONNECT_RETRY_PATH_4D39F2C1"
)]
pub struct RetryClient;

#[derive(Debug)]
#[provider(UnixConnect("target/sd-uds-connect-missing-peer.sock"), eager = true)]
pub struct MissingPeerClient;

#[service(tags = ["unix_connect_missing_peer_provider_test"])]
async fn missing_peer_client_service(
    _client: std::sync::Arc<MissingPeerClient>,
) -> anyhow::Result<()> {
    MISSING_PEER_SERVICE_ENTERED.store(true, Ordering::SeqCst);
    Ok(())
}

#[derive(Debug)]
#[provider(
    UnixConnect("target/sd-uds-connect-env-eager-fallback.sock"),
    env = "SERVICE_DAEMON_RS_UNIX_CONNECT_ENV_EAGER_PATH_8E16B4A9",
    eager = true
)]
pub struct EnvEagerClient;

#[service(tags = ["unix_connect_env_eager_provider_test"])]
async fn env_eager_client_service(client: std::sync::Arc<EnvEagerClient>) -> anyhow::Result<()> {
    ENV_EAGER_SERVICE_ENTERED.store(true, Ordering::SeqCst);

    if !ENV_EAGER_PROBE_ACCEPTED.load(Ordering::SeqCst) {
        let _ =
            tokio::time::timeout(Duration::from_secs(5), ENV_EAGER_PROBE_READY.notified()).await;
    }

    ENV_EAGER_SERVICE_SAW_PROBE.store(
        ENV_EAGER_PROBE_ACCEPTED.load(Ordering::SeqCst),
        Ordering::SeqCst,
    );
    ENV_EAGER_SERVICE_SAW_ENV_PATH
        .store(client.path() == Path::new(ENV_EAGER_PATH), Ordering::SeqCst);
    ENV_EAGER_SERVICE_READY.notify_one();

    service_daemon::done();
    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }

    Ok(())
}

// We need multi_thread so the delayed-bind task can run while the main task
// is parked inside init_fallible's backoff sleep. With current_thread the
// runtime would wait for the spawn to be polled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_unix_connect_missing_peer_returns_provider_init_error() {
    let path = "target/sd-uds-connect-missing-peer.sock";
    let _guard = prepare_socket_path(path);
    MISSING_PEER_SERVICE_ENTERED.store(false, Ordering::SeqCst);

    let daemon = ServiceDaemon::builder()
        .with_registry(
            service_daemon::Registry::builder()
                .with_tag("unix_connect_missing_peer_provider_test")
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
    assert!(!MISSING_PEER_SERVICE_ENTERED.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_unix_connect_env_overrides_fallback_and_eager_runs_before_service_body()
-> anyhow::Result<()> {
    ENV_EAGER_SERVICE_ENTERED.store(false, Ordering::SeqCst);
    ENV_EAGER_SERVICE_SAW_ENV_PATH.store(false, Ordering::SeqCst);
    ENV_EAGER_SERVICE_SAW_PROBE.store(false, Ordering::SeqCst);
    ENV_EAGER_PROBE_ACCEPTED.store(false, Ordering::SeqCst);

    let _env_var = set_test_env(ENV_EAGER_ENV_VAR, ENV_EAGER_PATH);
    let _fallback_guard = prepare_socket_path(ENV_EAGER_FALLBACK_PATH);
    let _env_path_guard = prepare_socket_path(ENV_EAGER_PATH);

    let peer = std::os::unix::net::UnixListener::bind(ENV_EAGER_PATH)?;
    peer.set_nonblocking(true)?;
    let peer = tokio::net::UnixListener::from_std(peer)?;
    let accept_task = tokio::spawn(async move {
        if peer.accept().await.is_ok() {
            ENV_EAGER_PROBE_ACCEPTED.store(true, Ordering::SeqCst);
            ENV_EAGER_PROBE_READY.notify_one();
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let daemon = ServiceDaemon::builder()
        .with_registry(
            service_daemon::Registry::builder()
                .with_tag("unix_connect_env_eager_provider_test")
                .build(),
        )
        .with_restart_policy(
            RestartPolicy::builder()
                .initial_delay(Duration::from_millis(1))
                .max_delay(Duration::from_millis(5))
                .jitter_factor(0.0)
                .provider_init_timeout(Duration::from_millis(200))
                .build(),
        )
        .build();
    let cancel = daemon.cancel_token();

    daemon.run().await;

    if !ENV_EAGER_SERVICE_ENTERED.load(Ordering::SeqCst) {
        tokio::time::timeout(Duration::from_secs(5), ENV_EAGER_SERVICE_READY.notified()).await?;
    }

    assert!(ENV_EAGER_SERVICE_ENTERED.load(Ordering::SeqCst));
    assert!(ENV_EAGER_SERVICE_SAW_ENV_PATH.load(Ordering::SeqCst));
    assert!(ENV_EAGER_SERVICE_SAW_PROBE.load(Ordering::SeqCst));

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), daemon.wait()).await??;
    accept_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_unix_connect_retries_on_connection_refused() {
    let path = unique_socket_path("connect-retry");
    let path_string = path
        .to_str()
        .expect("test socket path must be valid UTF-8")
        .to_owned();
    let _env_var = set_test_env(RETRY_ENV_VAR, &path_string);
    let _guard = prepare_owned_socket_path(path.clone());

    // Spawn a delayed peer that binds 200ms after the test starts. The
    // template's first probe will hit NotFound (Retryable); the framework
    // backs off and retries; by the time the second probe runs, the peer
    // should be up.
    let delayed_path = path.clone();
    let peer_handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let listener = std::os::unix::net::UnixListener::bind(&delayed_path)
            .expect("Delayed peer bind failed");
        // Hold the listener long enough for the test body to finish.
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        drop(listener);
    });

    let result = RetryClient::resolve().await;
    assert!(
        result.is_ok(),
        "Expected framework init to retry until delayed peer is ready, got {:?}",
        result
    );

    // We don't strictly need to await the peer task -- it cleans up via
    // drop() at the end of its sleep. Aborting keeps the test quick.
    peer_handle.abort();
}

// ---------------------------------------------------------------------------
// Each call to try_connect() yields an independent UnixStream.
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[provider(UnixConnect("target/sd-uds-connect-indep.sock"))]
pub struct IndepClient;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_unix_connect_provides_independent_streams_per_call() {
    let path = "target/sd-uds-connect-indep.sock";
    let _guard = prepare_socket_path(path);

    let _peer = std::os::unix::net::UnixListener::bind(path).expect("peer bind failed");

    let provider = <IndepClient as ManagedProvided>::resolve_managed()
        .await
        .expect("resolve_managed failed for IndepClient");

    // Open two streams. They must be independent -- writes/reads on one must
    // not affect the other. We don't have a full peer accept loop here, so
    // we settle for a structural check: both calls return Ok and the
    // returned objects have distinct OS file descriptors (proxied via
    // checking that we can hold both simultaneously without one closing
    // the other).
    let s1 = provider
        .try_connect()
        .await
        .expect("first try_connect failed");
    let s2 = provider
        .try_connect()
        .await
        .expect("second try_connect failed (peer accept queue exhausted?)");

    // Drop in reverse order to confirm independent lifetimes.
    drop(s2);
    drop(s1);
}

// ---------------------------------------------------------------------------
// Provider exposes the configured path via path() accessor.
// ---------------------------------------------------------------------------
//
// This is a smoke test that the generated path() helper exists and matches
// what was declared in the macro attribute. Useful for diagnostics and
// for users who want to log the target path without parsing Display output.

#[derive(Debug)]
#[provider(UnixConnect("target/sd-uds-connect-pathfn.sock"))]
pub struct PathFnClient;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_unix_connect_path_accessor_returns_configured_path() {
    let path = "target/sd-uds-connect-pathfn.sock";
    let _guard = prepare_socket_path(path);

    let _peer = std::os::unix::net::UnixListener::bind(path).expect("peer bind failed");

    let provider = <PathFnClient as ManagedProvided>::resolve_managed()
        .await
        .expect("resolve_managed failed");

    assert_eq!(
        provider.path(),
        std::path::Path::new(path),
        "path() should return the configured socket path"
    );
}

// ---------------------------------------------------------------------------
// connect() opens a fresh independent stream per call.
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[provider(UnixConnect("target/sd-uds-connect-convenience.sock"))]
pub struct ConvenienceClient;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_unix_connect_convenience_method_opens_independent_streams() {
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
