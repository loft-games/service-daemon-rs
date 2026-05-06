// End-to-end test: UnixListen and UnixConnect cooperating on the same path.
//
// This test exercises the documented two-modal interaction:
//   1. Peer servers will observe an accept() followed by an instant close
//      from UnixConnect's init-time probe. The application-layer accept loop
//      must therefore tolerate "connect-then-close" patterns.
//   2. After the probe, UnixConnect::try_connect() opens a fresh independent
//      stream for actual business traffic.
//
// The test does NOT spin up a full ServiceDaemon -- it directly resolves
// both providers and drives accept/connect manually. This keeps the test
// focused on the provider contract; daemon-level integration would belong
// in a separate examples-based smoke test.
//
// Windows-handoff note: `#![cfg(unix)]`-gated; not compiled on Windows.

#![cfg(unix)]

use service_daemon::{ManagedProvided, provider};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn cleanup_path(path: &str) {
    match std::fs::remove_file(path) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("cleanup_path({}) failed: {}", path, e),
    }
}

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

// Both providers point at the same path. In real usage they would belong to
// different services (one running the accept loop, one acting as a client).
#[derive(Debug)]
#[provider(UnixListen("target/sd-uds-rt.sock"))]
pub struct RtServer;

#[derive(Debug)]
#[provider(UnixConnect("target/sd-uds-rt.sock"))]
pub struct RtClient;

// multi_thread: the server task and the client task must make progress
// independently. With current_thread the test would deadlock on the server's
// accept().await while the main task was inside the client's connect().
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_unix_listen_connect_roundtrip() {
    let path = "target/sd-uds-rt.sock";
    let _guard = prepare_socket_path(path);

    // Bring up the listener provider FIRST, so the path exists before the
    // client provider's init-probe runs. Without this ordering the client
    // would have to retry through NotFound until init_fallible's backoff
    // catches the late bind -- functional but slower than necessary for a
    // tight roundtrip test.
    let server = <RtServer as ManagedProvided>::resolve_managed()
        .await
        .expect("RtServer resolve failed");

    let listener = server.try_get().await.expect("RtServer.try_get failed");

    // Spawn the server-side accept loop. It expects exactly two connections:
    //   1. The probe from RtClient's init-time UnixStream::connect, which is
    //      dropped immediately.
    //   2. The real roundtrip stream from try_connect().
    let server_task = tokio::spawn(async move {
        // Connection 1: probe from UnixConnect init. We accept it and drop
        // it. The peer (UnixConnect) drops its end immediately too.
        let (probe, _) = listener
            .accept()
            .await
            .expect("Failed to accept the init-time probe connection");
        drop(probe);

        // Connection 2: the actual business traffic.
        let (mut sock, _) = listener
            .accept()
            .await
            .expect("Failed to accept the real roundtrip connection");

        let mut buf = [0u8; 5];
        sock.read_exact(&mut buf)
            .await
            .expect("Server read_exact failed");
        sock.write_all(b"world")
            .await
            .expect("Server write_all failed");
        buf
    });

    // Resolve the client provider. This performs the init-time probe (which
    // the server task accepts as connection #1).
    let client = <RtClient as ManagedProvided>::resolve_managed()
        .await
        .expect("RtClient resolve failed");

    // Open the real roundtrip stream (connection #2).
    let mut conn = client
        .try_connect()
        .await
        .expect("RtClient.try_connect failed");

    conn.write_all(b"hello")
        .await
        .expect("Client write_all failed");

    let mut response = [0u8; 5];
    conn.read_exact(&mut response)
        .await
        .expect("Client read_exact failed");

    assert_eq!(&response, b"world", "client should receive 'world'");

    let received = server_task.await.expect("Server task panicked");
    assert_eq!(&received, b"hello", "server should receive 'hello'");
}
