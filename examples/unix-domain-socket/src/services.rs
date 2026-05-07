use crate::providers::{
    EXAMPLE_UNIX_DOMAIN_SOCKET_PATH, ExampleUnixDomainSocketConnector,
    ExampleUnixDomainSocketListener,
};
use service_daemon::{ServicePriority, service};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::info;

#[service(priority = ServicePriority::STORAGE)]
pub async fn unix_domain_socket_server_service(
    listener: Arc<ExampleUnixDomainSocketListener>,
) -> anyhow::Result<()> {
    service_daemon::done();

    let (probe_connection, _) = listener.accept().await?;
    drop(probe_connection);
    info!(
        path = EXAMPLE_UNIX_DOMAIN_SOCKET_PATH,
        "Accepted and closed the connector initialization probe"
    );

    let (mut business_connection, _) = listener.accept().await?;
    let mut request = [0_u8; 5];
    business_connection.read_exact(&mut request).await?;
    info!(
        request = %String::from_utf8_lossy(&request),
        "Received Unix domain socket request"
    );

    business_connection.write_all(b"world").await?;
    info!("Sent Unix domain socket response");

    service_daemon::wait_shutdown().await;
    Ok(())
}

#[service]
pub async fn unix_domain_socket_client_service(
    connector: Arc<ExampleUnixDomainSocketConnector>,
) -> anyhow::Result<()> {
    let mut connection = connector.connect().await?;
    connection.write_all(b"hello").await?;

    let mut response = [0_u8; 5];
    connection.read_exact(&mut response).await?;
    info!(
        response = %String::from_utf8_lossy(&response),
        "Received Unix domain socket response"
    );

    service_daemon::done();
    service_daemon::wait_shutdown().await;
    Ok(())
}
