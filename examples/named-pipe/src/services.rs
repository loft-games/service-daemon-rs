//! Services that exercise the Windows named pipe provider templates.

use crate::providers::{
    EXAMPLE_NAMED_PIPE_NAME, ExampleNamedPipeConnector, ExampleNamedPipeListener,
};
use service_daemon::{ServicePriority, service};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::info;

#[service(priority = ServicePriority::STORAGE)]
pub async fn named_pipe_server_service(
    listener: Arc<ExampleNamedPipeListener>,
) -> anyhow::Result<()> {
    service_daemon::done();

    let probe_connection = listener.accept().await?;
    drop(probe_connection);
    info!(
        pipe = EXAMPLE_NAMED_PIPE_NAME,
        "Accepted and closed the connector initialization probe"
    );

    let mut business_connection = listener.accept().await?;
    let mut request = [0_u8; 5];
    business_connection.read_exact(&mut request).await?;
    info!(
        request = %String::from_utf8_lossy(&request),
        "Received Windows named pipe request"
    );

    business_connection.write_all(b"world").await?;
    info!("Sent Windows named pipe response");

    service_daemon::wait_shutdown().await;
    Ok(())
}

#[service]
pub async fn named_pipe_client_service(
    connector: Arc<ExampleNamedPipeConnector>,
) -> anyhow::Result<()> {
    let mut connection = connector.connect().await?;
    connection.write_all(b"hello").await?;

    let mut response = [0_u8; 5];
    connection.read_exact(&mut response).await?;
    info!(
        response = %String::from_utf8_lossy(&response),
        "Received Windows named pipe response"
    );

    service_daemon::done();
    service_daemon::wait_shutdown().await;
    Ok(())
}
