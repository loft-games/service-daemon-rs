// Integration tests for the `#[provider(UnixConnect("..."))]` template.
//
// See `unix_listen_strategy_tests.rs` for the rationale on per-test struct +
// path layout, RAII path guards, and Windows-handoff caveats. The same
// constraints apply here.

#![cfg(unix)]

use service_daemon::{ManagedProvided, provider};

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

// ---------------------------------------------------------------------------
// Test 1: connect succeeds when the peer is already listening at init time.
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[provider(UnixConnect("target/sd-uds-connect-ok.sock"))]
pub struct OkClient;

#[tokio::test]
async fn test_unix_connect_succeeds_when_server_ready() {
    let path = "target/sd-uds-connect-ok.sock";
    cleanup_path(path);
    let _guard = PathGuard(path);

    // Bring up a peer listener BEFORE the provider resolves. This satisfies
    // the init-time probe on the first attempt (no retry/backoff path
    // exercised here -- that's test 2's job).
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
// Test 2: connect retries through ConnectionRefused/NotFound until the peer
//         shows up, then succeeds. Validates the Retryable classification.
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[provider(UnixConnect("target/sd-uds-connect-retry.sock"))]
pub struct RetryClient;

// We need multi_thread so the delayed-bind task can run while the main task
// is parked inside init_fallible's backoff sleep. With current_thread the
// runtime would wait for the spawn to be polled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_unix_connect_retries_on_connection_refused() {
    let path = "target/sd-uds-connect-retry.sock";
    cleanup_path(path);
    let _guard = PathGuard(path);

    // Spawn a delayed peer that binds 200ms after the test starts. The
    // template's first probe will hit NotFound (Retryable); the framework
    // backs off and retries; by the time the second probe runs, the peer
    // should be up.
    let delayed_path = path.to_owned();
    let peer_handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let listener = std::os::unix::net::UnixListener::bind(&delayed_path)
            .expect("Delayed peer bind failed");
        // Hold the listener long enough for the test body to finish.
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        drop(listener);
    });

    let result = <RetryClient as ManagedProvided>::resolve_managed().await;
    assert!(
        result.is_ok(),
        "Expected eventual connect success after delayed peer, got {:?}",
        result
    );

    // We don't strictly need to await the peer task -- it cleans up via
    // drop() at the end of its sleep. Aborting keeps the test quick.
    peer_handle.abort();
}

// ---------------------------------------------------------------------------
// Test 3: each call to try_connect() yields an independent UnixStream.
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[provider(UnixConnect("target/sd-uds-connect-indep.sock"))]
pub struct IndepClient;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_unix_connect_provides_independent_streams_per_call() {
    let path = "target/sd-uds-connect-indep.sock";
    cleanup_path(path);
    let _guard = PathGuard(path);

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
// Test 4: provider exposes the configured path via path() accessor.
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
    cleanup_path(path);
    let _guard = PathGuard(path);

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
