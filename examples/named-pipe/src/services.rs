//! Services that exercise the Windows named pipe provider templates.

use crate::models::error::NamedPipeError;
use crate::providers::{
    EXAMPLE_NAMED_PIPE_NAME, ExampleNamedPipeConnector, ExampleNamedPipeListener,
};
use service_daemon::{ServicePriority, service};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::NamedPipeClient;
use tracing::info;

const ERROR_PIPE_BUSY: i32 = 231;
const REQUEST_PAYLOAD: &[u8] = b"hello";
const RESPONSE_PAYLOAD: &[u8] = b"world";

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

    {
        // `NamedPipeConnect` opens one initialization probe connection and then
        // closes its client end. This block accepts only that probe; the server
        // side closes naturally at the end of the block without reading data.
        let _probe_connection = match listener.accept().await {
            Ok(connection) => connection,
            Err(source) => {
                return Err(NamedPipeError::AcceptInitializationProbe {
                    name: EXAMPLE_NAMED_PIPE_NAME,
                    source,
                }
                .into());
            }
        };
        info!(
            pipe = EXAMPLE_NAMED_PIPE_NAME,
            "Accepted and closed the connector initialization probe"
        );
    }

    let mut business_connection = match listener.accept().await {
        Ok(connection) => connection,
        Err(source) => {
            return Err(NamedPipeError::AcceptBusinessConnection {
                name: EXAMPLE_NAMED_PIPE_NAME,
                source,
            }
            .into());
        }
    };
    let mut request = [0_u8; REQUEST_PAYLOAD.len()];
    if let Err(source) = business_connection.read_exact(&mut request).await {
        return Err(NamedPipeError::ReadRequest {
            name: EXAMPLE_NAMED_PIPE_NAME,
            source,
        }
        .into());
    }
    info!(
        request = %String::from_utf8_lossy(&request),
        "Received Windows named pipe request"
    );

    if let Err(source) = business_connection.write_all(RESPONSE_PAYLOAD).await {
        return Err(NamedPipeError::WriteResponse {
            name: EXAMPLE_NAMED_PIPE_NAME,
            source,
        }
        .into());
    }
    info!("Sent Windows named pipe response");

    service_daemon::wait_shutdown().await;
    Ok(())
}

#[service]
pub async fn named_pipe_client_service(
    connector: Arc<ExampleNamedPipeConnector>,
) -> anyhow::Result<()> {
    let mut connection = match connect_with_busy_retry(&connector).await {
        Ok(connection) => connection,
        Err(source) => {
            return Err(NamedPipeError::ConnectClient {
                name: EXAMPLE_NAMED_PIPE_NAME,
                source,
            }
            .into());
        }
    };
    if let Err(source) = connection.write_all(REQUEST_PAYLOAD).await {
        return Err(NamedPipeError::WriteRequest {
            name: EXAMPLE_NAMED_PIPE_NAME,
            source,
        }
        .into());
    }

    let mut response = [0_u8; RESPONSE_PAYLOAD.len()];
    if let Err(source) = connection.read_exact(&mut response).await {
        return Err(NamedPipeError::ReadResponse {
            name: EXAMPLE_NAMED_PIPE_NAME,
            source,
        }
        .into());
    }
    info!(
        response = %String::from_utf8_lossy(&response),
        "Received Windows named pipe response"
    );

    service_daemon::done();
    service_daemon::wait_shutdown().await;
    Ok(())
}
