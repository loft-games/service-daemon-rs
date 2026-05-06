// Integration tests for the `#[provider(UnixListen("..."))]` template.
//
// Why each test uses its own provider struct + path: provider singletons are
// per-type statics (see generate_provided_impl). Sharing a struct across
// tests would let the first resolve cache the result for everyone, masking
// the very behaviors these tests are meant to exercise.
//
// Path strategy: hardcoded relative paths under `target/`. Cargo's test cwd
// is the crate root, so the test helper creates `target/` before binding.
// `cargo clean` reclaims any stragglers.
//
// Windows-handoff note: this file is `#![cfg(unix)]` so on Windows it is
// excluded entirely from compilation -- including syntactic checks. If a
// macOS / Linux contributor hits a compile error here that wasn't caught on
// Windows, that is expected: source-level review is the only Windows-side
// validation possible for this file.

#![cfg(unix)]

use service_daemon::{ManagedProvided, ProviderError, provider};

/// Best-effort cleanup of a socket file path. NotFound is benign (path may
/// already be gone). Other errors are logged but not propagated -- this is
/// a test helper, not a production guard.
fn cleanup_path(path: &str) {
    match std::fs::remove_file(path) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("cleanup_path({}) failed: {} (ignored in test)", path, e),
    }
}

/// RAII drop guard that removes a socket file when the guard goes out of
/// scope, including on panic. Without this, a panicking test would leave a
/// stale socket file that the next run would have to detect-and-unlink.
struct PathGuard(&'static str);
impl Drop for PathGuard {
    fn drop(&mut self) {
        cleanup_path(self.0);
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

// ---------------------------------------------------------------------------
// Test 1: fresh-path bind succeeds.
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[provider(UnixListen("target/sd-uds-listen-fresh.sock"))]
pub struct FreshListener;

#[tokio::test]
async fn test_unix_listen_fresh_path_ok() {
    let _guard = prepare_socket_path("target/sd-uds-listen-fresh.sock");

    let result = <FreshListener as ManagedProvided>::resolve_managed().await;
    assert!(
        result.is_ok(),
        "Expected fresh-path bind to succeed, got {:?}",
        result
    );
}

// ---------------------------------------------------------------------------
// Test 2: stale socket file is detected, unlinked, and bind succeeds.
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[provider(UnixListen("target/sd-uds-listen-stale.sock"))]
pub struct StaleListener;

#[tokio::test]
async fn test_unix_listen_stale_file_recovers() {
    let path = "target/sd-uds-listen-stale.sock";
    let _guard = prepare_socket_path(path);

    // Pre-create a regular file at the path. The probe `connect()` will fail
    // (it's a regular file, not a socket), so the template should classify
    // it as stale and unlink before binding.
    std::fs::write(path, b"stale data from a prior unclean shutdown")
        .expect("Failed to pre-create stale file");

    let result = <StaleListener as ManagedProvided>::resolve_managed().await;
    assert!(
        result.is_ok(),
        "Expected stale-file recovery to bind successfully, got {:?}",
        result
    );
}

// ---------------------------------------------------------------------------
// Test 3: a live process holding the path is refused fatally.
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[provider(UnixListen("target/sd-uds-listen-live.sock"))]
pub struct LiveListener;

#[tokio::test]
async fn test_unix_listen_live_process_refuses() {
    let path = "target/sd-uds-listen-live.sock";
    let _guard = prepare_socket_path(path);

    // Bind a manual std listener and keep it alive for the duration of the
    // test. Our template's connect-probe will succeed against this live
    // listener, triggering the "held by another live process" Fatal path.
    let _live = std::os::unix::net::UnixListener::bind(path)
        .expect("Failed to start the live conflicting listener");

    let result = <LiveListener as ManagedProvided>::resolve_managed().await;
    match result {
        Err(ProviderError::Fatal(msg)) => {
            assert!(
                msg.contains("held by another live process"),
                "Expected 'held by another live process' in Fatal msg, got: {}",
                msg
            );
        }
        other => panic!(
            "Expected Fatal 'held by another live process', got {:?}",
            other
        ),
    }
}

// ---------------------------------------------------------------------------
// Test 4: permission-denied bind is Fatal (not retried into a long timeout).
// ---------------------------------------------------------------------------

// `/proc/sd-uds-perm.sock`: /proc is typically not writable for unprivileged
// users on Linux. If we are running as root or on a system that allows it,
// the test self-skips by detecting that the bind would actually succeed.
#[derive(Debug)]
#[provider(UnixListen("/proc/sd-uds-perm.sock"))]
pub struct PermListener;

#[tokio::test]
async fn test_unix_listen_permission_denied_fatal() {
    // Self-skip guard: if a real bind succeeds (root, or a system where /proc
    // is writable), the Fatal path won't trigger, so skip cleanly.
    if let Ok(probe) = std::os::unix::net::UnixListener::bind("/proc/sd-uds-perm.sock") {
        drop(probe);
        let _ = std::fs::remove_file("/proc/sd-uds-perm.sock");
        return;
    }

    let result = <PermListener as ManagedProvided>::resolve_managed().await;
    match result {
        Err(ProviderError::Fatal(_)) => {}
        other => panic!("Expected Fatal on permission-denied path, got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// Test 5: try_get clones the underlying FD; multiple clones coexist.
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[provider(UnixListen("target/sd-uds-listen-clone.sock"))]
pub struct CloneListener;

#[tokio::test]
async fn test_unix_listen_fd_clone() {
    let path = "target/sd-uds-listen-clone.sock";
    let _guard = prepare_socket_path(path);

    let provider = <CloneListener as ManagedProvided>::resolve_managed()
        .await
        .expect("resolve_managed failed for CloneListener");

    // try_get clones via dup(); both must yield valid tokio listeners.
    let l1 = provider.try_get().await.expect("first try_get failed");
    let l2 = provider
        .try_get()
        .await
        .expect("second try_get failed (FD exhaustion?)");

    // Both clones share the kernel listen queue. We don't drive accept() in
    // this test (would require an external connector) -- the assertion is
    // simply that two clones can be obtained without error and their
    // local_addr resolves to the same path.
    let a1 = l1.local_addr().expect("clone-1 local_addr failed");
    let a2 = l2.local_addr().expect("clone-2 local_addr failed");
    assert_eq!(
        a1.as_pathname(),
        a2.as_pathname(),
        "clones must share the same bound pathname"
    );
}
