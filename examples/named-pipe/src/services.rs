//! Demonstrates the server-side probe accept and a separate business connection.

use crate::providers::{ExampleNamedPipeConnector, ExampleNamedPipeListener};
use service_daemon::{ServicePriority, service};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::info;

const ERROR_PIPE_BUSY: i32 = 231;

async fn connect_with_busy_retry(
    connector: &ExampleNamedPipeConnector,
) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    for _ in 0..50 {
        match connector.connect().await {
            Ok(connection) => return Ok(connection),
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) => return Err(error),
        }
    }

    connector.connect().await
}

#[service(priority = ServicePriority::STORAGE)]
pub async fn named_pipe_server_service(
    listener: std::sync::Arc<ExampleNamedPipeListener>,
) -> anyhow::Result<()> {
    service_daemon::done();

    let probe_connection = listener.accept().await?;
    drop(probe_connection);
    info!(
        pipe = listener.name(),
        "Accepted and closed the connector initialization probe"
    );

    let mut business_connection = listener.accept().await?;
    let mut request = [0_u8; 5];
    business_connection.read_exact(&mut request).await?;
    info!(
        request = %String::from_utf8_lossy(&request),
        "Received named pipe request"
    );

    business_connection.write_all(b"world").await?;
    info!("Sent named pipe response");

    service_daemon::wait_shutdown().await;
    Ok(())
}

#[service]
pub async fn named_pipe_client_service(
    connector: std::sync::Arc<ExampleNamedPipeConnector>,
) -> anyhow::Result<()> {
    let mut connection = connect_with_busy_retry(&connector).await?;
    connection.write_all(b"hello").await?;

    let mut response = [0_u8; 5];
    connection.read_exact(&mut response).await?;
    info!(
        response = %String::from_utf8_lossy(&response),
        "Received named pipe response"
    );

    service_daemon::done();
    service_daemon::wait_shutdown().await;
    Ok(())
}
