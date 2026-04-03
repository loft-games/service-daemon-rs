use service_daemon::{ManagedProvided, ProviderError};
use service_daemon_macro::provider;

#[derive(Debug)]
#[provider(Listen("127.0.0.1:28083"))]
pub struct ConflictListener;

#[tokio::test]
async fn test_listen_addr_resolution() {
    // Direct test: resolve the provider and check success.
    // By using a free high port, we ensure no fatal provider error is returned.
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

    let result = <RootListener as ManagedProvided>::resolve_managed().await;
    assert!(
        matches!(result, Err(ProviderError::Fatal(_))),
        "Expected fatal provider error on privileged port 80, got {:?}",
        result
    );
}
