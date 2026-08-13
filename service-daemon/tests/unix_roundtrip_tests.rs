// End-to-end test: UnixListen and UnixConnect cooperating on the same path.
//
// This test exercises the common interaction: the listener accepts one stream
// and the connector opens one fresh business connection.
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

    let server = <RtServer as ManagedProvided>::resolve_managed()
        .await
        .expect("RtServer resolve failed");

    let server_task = tokio::spawn(async move {
        let mut sock = server
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

    let client = <RtClient as ManagedProvided>::resolve_managed()
        .await
        .expect("RtClient resolve failed");

    let mut conn = client.connect().await.expect("RtClient.connect failed");

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
