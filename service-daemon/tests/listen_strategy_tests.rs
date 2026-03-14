use service_daemon::ManagedProvided;
use service_daemon_macro::provider;

#[derive(Debug)]
#[provider(Listen("127.0.0.1:28083"))]
pub struct ConflictListener;

#[tokio::test]
async fn test_listen_addr_resolution() {
    // Direct test: resolve the provider and check success.
    // By using a free high port, we ensure no FATAL panic is triggered.
    let result = <ConflictListener as ManagedProvided>::resolve_managed().await;

    assert!(
        result.is_ok(),
        "Expected successful resolution on high port, got {:?}",
        result
    );
}

#[derive(Debug)]
#[provider(Listen("127.0.0.1:80"))]
pub struct RootListener;

#[tokio::test]
async fn test_listen_permission_denied_fatal() {
    // Guard: if running with root privileges, port 80 binds successfully.
    // We skip the test in this case as the Fatal path won't trigger.
    if std::net::TcpListener::bind("127.0.0.1:80").is_ok() {
        return;
    }

    // With the exit-to-panic refactor, provider_init_exit now panics.
    // Spawn in a separate task to catch the panic via JoinHandle.
    let handle = tokio::spawn(async {
        let _ = <RootListener as ManagedProvided>::resolve_managed().await;
    });

    let result = handle.await;
    assert!(
        result.is_err(),
        "Expected Fatal panic on privileged port 80, but the provider succeeded"
    );
}
