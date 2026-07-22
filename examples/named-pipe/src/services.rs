//! Services that exercise the Windows named pipe provider templates.

use crate::providers::{
    EXAMPLE_NAMED_PIPE_NAME, ExampleNamedPipeConnector, ExampleNamedPipeListener,
};
use service_daemon::{ServicePriority, service};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::NamedPipeClient;
use tracing::info;

const ERROR_PIPE_BUSY: i32 = 231;

async fn connect_with_busy_retry(
    connector: &ExampleNamedPipeConnector,
) -> std::io::Result<NamedPipeClient> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match connector.connect().await {
            Ok(connection) => return Ok(connection),
            Err(error)
                if error.raw_os_error() == Some(ERROR_PIPE_BUSY)
                    && std::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

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
    let mut connection = connect_with_busy_retry(&connector).await?;
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
