// Windows-only integration tests for the `#[provider(NamedPipeListen(...))]`
// and `#[provider(NamedPipeConnect(...))]` templates.
//
// Each test uses its own provider type because generated provider root slots
// are per-type statics. Env overrides provide per-process unique pipe names
// while preserving string-literal macro syntax.

#![cfg(windows)]

use service_daemon::{ManagedProvided, ProviderError, provider};
use std::ffi::OsString;
use std::sync::LazyLock;
use std::time::Duration;
use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};
use tokio::sync::Mutex as AsyncMutex;

static ENV_VAR_LOCK: LazyLock<AsyncMutex<()>> = LazyLock::new(|| AsyncMutex::new(()));

const SERVER_NAME_ENV: &str = "SERVICE_DAEMON_RS_NAMED_PIPE_SERVER_NAME_5F30D1E2";
const OWNERSHIP_NAME_ENV: &str = "SERVICE_DAEMON_RS_NAMED_PIPE_OWNERSHIP_NAME_F1F7F85D";
const OK_CLIENT_NAME_ENV: &str = "SERVICE_DAEMON_RS_NAMED_PIPE_OK_CLIENT_NAME_633E6C20";
const BUSY_RETRY_NAME_ENV: &str = "SERVICE_DAEMON_RS_NAMED_PIPE_BUSY_RETRY_NAME_169B58D0";
const MISSING_PEER_NAME_ENV: &str = "SERVICE_DAEMON_RS_NAMED_PIPE_MISSING_NAME_DDF1F1E9";
const ENV_EAGER_NAME_ENV: &str = "SERVICE_DAEMON_RS_NAMED_PIPE_ENV_EAGER_NAME_466CF2DE";
const ACCEPT_CANCEL_NAME_ENV: &str = "SERVICE_DAEMON_RS_NAMED_PIPE_ACCEPT_CANCEL_NAME_8D3954F2";
const ACCEPT_REPLACEMENT_FAIL_NAME_ENV: &str =
    "SERVICE_DAEMON_RS_NAMED_PIPE_ACCEPT_REPLACEMENT_FAIL_NAME_9F31E5B1";

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
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

fn create_server(name: &str) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    ServerOptions::new().create(name)
}

fn create_single_instance_server(
    name: &str,
) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    let mut options = ServerOptions::new();
    options.max_instances(1);
    options.create(name)
}

async fn open_client_with_retry(
    name: &str,
) -> anyhow::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match ClientOptions::new().open(name) {
            Ok(client) => return Ok(client),
            Err(error)
                if (error.kind() == std::io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(231))
                    && std::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

#[derive(Debug)]
#[provider(
    NamedPipeListen(r"\\.\pipe\service-daemon-rs-server-fallback"),
    env = "SERVICE_DAEMON_RS_NAMED_PIPE_SERVER_NAME_5F30D1E2"
)]
pub struct NamedPipeServerProvider;

#[tokio::test]
async fn named_pipe_listen_creates_server_and_exposes_name() {
    let _env_lock = ENV_VAR_LOCK.lock().await;
    let name = unique_pipe_name("server-name");
    let _env = set_test_env(SERVER_NAME_ENV, &name);

    let provider = <NamedPipeServerProvider as ManagedProvided>::resolve_managed()
        .await
        .expect("NamedPipeServerProvider resolve failed");

    assert_eq!(provider.name(), name);
    assert_eq!(provider.to_string(), name);
}

#[derive(Debug)]
#[provider(
    NamedPipeListen(r"\\.\pipe\service-daemon-rs-accept-cancel-fallback"),
    env = "SERVICE_DAEMON_RS_NAMED_PIPE_ACCEPT_CANCEL_NAME_8D3954F2"
)]
pub struct AcceptCancellationServer;
#[derive(Debug)]
#[provider(
    NamedPipeListen(r"\\.\pipe\service-daemon-rs-accept-replacement-fail-fallback"),
    env = "SERVICE_DAEMON_RS_NAMED_PIPE_ACCEPT_REPLACEMENT_FAIL_NAME_9F31E5B1"
)]
pub struct AcceptReplacementFailureServer;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn named_pipe_listen_recovers_after_cancelled_accept_wait() -> anyhow::Result<()> {
    let _env_lock = ENV_VAR_LOCK.lock().await;
    let name = unique_pipe_name("accept-cancel");
    let _env = set_test_env(ACCEPT_CANCEL_NAME_ENV, &name);

    let provider = <AcceptCancellationServer as ManagedProvided>::resolve_managed()
        .await
        .expect("AcceptCancellationServer resolve failed");

    let first_accept = tokio::time::timeout(Duration::from_millis(20), provider.accept()).await;
    assert!(
        first_accept.is_err(),
        "first accept should be cancelled by timeout"
    );

    let accept_provider = std::sync::Arc::clone(&provider);
    let accept_task = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(5), accept_provider.accept())
            .await
            .expect("second accept timed out")
            .expect("second accept failed")
    });

    tokio::time::sleep(Duration::from_millis(20)).await;
    let client = ClientOptions::new().open(&name)?;
    let server = accept_task.await.expect("accept task panicked");

    drop(client);
    drop(server);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn named_pipe_listen_delivers_connection_while_replacement_retries() -> anyhow::Result<()> {
    let _env_lock = ENV_VAR_LOCK.lock().await;
    let name = unique_pipe_name("accept-replacement-fail");
    let _env = set_test_env(ACCEPT_REPLACEMENT_FAIL_NAME_ENV, &name);

    let provider = std::sync::Arc::new(
        AcceptReplacementFailureServer::try_new_with_max_instances_for_test(1)
            .expect("AcceptReplacementFailureServer resolve failed"),
    );

    let client = ClientOptions::new().open(&name)?;

    let accept_result = provider.accept().await;
    assert!(
        accept_result.is_ok(),
        "accept should deliver the connected server while replacement retries internally"
    );
    let server = accept_result?;

    drop(client);
    drop(server);

    let accept_provider = std::sync::Arc::clone(&provider);
    let accept_task = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(5), accept_provider.accept())
            .await
            .expect("recovered accept timed out")
            .expect("recovered accept failed")
    });

    let recovered_client = open_client_with_retry(&name).await?;
    let recovered_server = accept_task.await.expect("recovered accept task panicked");

    drop(recovered_client);
    drop(recovered_server);
    Ok(())
}

#[derive(Debug)]
#[provider(
    NamedPipeListen(r"\\.\pipe\service-daemon-rs-ownership-fallback"),
    env = "SERVICE_DAEMON_RS_NAMED_PIPE_OWNERSHIP_NAME_F1F7F85D"
)]
pub struct OwnershipCollisionServer;

#[tokio::test]
async fn named_pipe_listen_first_instance_collision_is_fatal() {
    let _env_lock = ENV_VAR_LOCK.lock().await;
    let name = unique_pipe_name("ownership-collision");
    let _env = set_test_env(OWNERSHIP_NAME_ENV, &name);
    let _existing = create_server(&name).expect("pre-existing server create failed");

    let result = <OwnershipCollisionServer as ManagedProvided>::resolve_managed().await;
    match result {
        Err(ProviderError::Fatal(message)) => {
            assert!(
                message.contains("first Windows named pipe server instance"),
                "unexpected fatal message: {message}"
            );
        }
        other => panic!("expected fatal first-instance collision, got {other:?}"),
    }
}

#[derive(Debug)]
#[provider(NamedPipeListen(r"\\server\pipe\remote"))]
pub struct RemoteServerName;

#[derive(Debug)]
#[provider(NamedPipeConnect(r"\\.\pipe\"))]
pub struct EmptyClientName;

#[tokio::test]
async fn named_pipe_templates_reject_invalid_local_names() {
    match RemoteServerName::try_new() {
        Err(ProviderError::Fatal(message)) => assert!(message.contains("local Windows named pipe")),
        other => panic!("expected fatal remote server name, got {other:?}"),
    }

    match EmptyClientName::try_new().await {
        Err(ProviderError::Fatal(message)) => assert!(message.contains("local Windows named pipe")),
        other => panic!("expected fatal empty client name, got {other:?}"),
    }
}

#[derive(Debug)]
#[provider(
    NamedPipeConnect(r"\\.\pipe\service-daemon-rs-ok-client-fallback"),
    env = "SERVICE_DAEMON_RS_NAMED_PIPE_OK_CLIENT_NAME_633E6C20"
)]
pub struct ReadyClient;

#[tokio::test]
async fn named_pipe_connect_resolves_without_peer() {
    let _env_lock = ENV_VAR_LOCK.lock().await;
    let name = unique_pipe_name("resolve-without-peer");
    let _env = set_test_env(OK_CLIENT_NAME_ENV, &name);

    let provider = <ReadyClient as ManagedProvided>::resolve_managed()
        .await
        .expect("NamedPipeConnect provider should resolve without dialing the peer");
    assert_eq!(provider.name(), name);
}

#[derive(Debug)]
#[provider(
    NamedPipeConnect(r"\\.\pipe\service-daemon-rs-busy-retry-fallback"),
    env = "SERVICE_DAEMON_RS_NAMED_PIPE_BUSY_RETRY_NAME_169B58D0"
)]
pub struct BusyRetryClient;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn named_pipe_connect_retries_busy_pipe_until_instance_available() {
    let _env_lock = ENV_VAR_LOCK.lock().await;
    let name = unique_pipe_name("busy-retry");
    let _env = set_test_env(BUSY_RETRY_NAME_ENV, &name);

    let busy_server = create_single_instance_server(&name).expect("busy server create failed");
    let busy_client = ClientOptions::new()
        .open(&name)
        .expect("busy holder client open failed");

    let provider = <BusyRetryClient as ManagedProvided>::resolve_managed()
        .await
        .expect("NamedPipeConnect provider should resolve without dialing the peer");

    let release_name = name.clone();
    let release_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(busy_client);
        drop(busy_server);
        let replacement =
            create_single_instance_server(&release_name).expect("replacement server create failed");
        tokio::time::sleep(Duration::from_secs(5)).await;
        drop(replacement);
    });

    let result = provider.connect().await;
    assert!(
        result.is_ok(),
        "expected runtime busy retry to recover, got {result:?}"
    );
    release_task.abort();
}

#[derive(Debug)]
#[provider(
    NamedPipeConnect(r"\\.\pipe\service-daemon-rs-missing-fallback"),
    env = "SERVICE_DAEMON_RS_NAMED_PIPE_MISSING_NAME_DDF1F1E9",
    eager = true
)]
pub struct MissingPeerClient;

#[tokio::test]
async fn named_pipe_missing_peer_errors_on_connect_call() {
    let _env_lock = ENV_VAR_LOCK.lock().await;
    let name = unique_pipe_name("missing-peer");
    let _env = set_test_env(MISSING_PEER_NAME_ENV, &name);

    let provider = <MissingPeerClient as ManagedProvided>::resolve_managed()
        .await
        .expect("NamedPipeConnect provider should resolve without dialing the peer");

    let error = provider
        .connect()
        .await
        .expect_err("connect() should report the missing peer at the call site");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}

#[derive(Debug)]
#[provider(
    NamedPipeConnect(r"\\.\pipe\service-daemon-rs-env-eager-fallback"),
    env = "SERVICE_DAEMON_RS_NAMED_PIPE_ENV_EAGER_NAME_466CF2DE",
    eager = true
)]
pub struct EnvEagerClient;

#[tokio::test]
async fn named_pipe_env_overrides_fallback_without_peer_probe() {
    let _env_lock = ENV_VAR_LOCK.lock().await;
    let name = unique_pipe_name("env-eager");
    let _env = set_test_env(ENV_EAGER_NAME_ENV, &name);

    let provider = <EnvEagerClient as ManagedProvided>::resolve_managed()
        .await
        .expect("env override should resolve without requiring a listening peer");

    assert_eq!(provider.name(), name);
}
