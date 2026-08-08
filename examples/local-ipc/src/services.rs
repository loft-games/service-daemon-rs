//! Services that exercise the cross-platform local IPC provider templates.

use crate::models::error::LocalIpcError;
use crate::providers::{EXAMPLE_LOCAL_IPC_NAME, ExampleLocalIpcConnector, ExampleLocalIpcListener};
use service_daemon::{ServicePriority, service};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tracing::info;

#[cfg(windows)]
use tokio::net::windows::named_pipe::NamedPipeClient;

const REQUEST_PAYLOAD: &[u8] = b"hello";
const RESPONSE_PAYLOAD: &[u8] = b"world";

#[cfg(windows)]
const ERROR_PIPE_BUSY: i32 = 231;

async fn read_request<S>(stream: &mut S) -> std::io::Result<[u8; REQUEST_PAYLOAD.len()]>
where
    S: AsyncRead + Unpin,
{
    let mut request = [0_u8; REQUEST_PAYLOAD.len()];
    stream.read_exact(&mut request).await?;
    Ok(request)
}

async fn write_response<S>(stream: &mut S) -> std::io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    stream.write_all(RESPONSE_PAYLOAD).await
}

async fn write_request<S>(stream: &mut S) -> std::io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    stream.write_all(REQUEST_PAYLOAD).await
}

async fn read_response<S>(stream: &mut S) -> std::io::Result<[u8; RESPONSE_PAYLOAD.len()]>
where
    S: AsyncRead + Unpin,
{
    let mut response = [0_u8; RESPONSE_PAYLOAD.len()];
    stream.read_exact(&mut response).await?;
    Ok(response)
}

#[cfg(windows)]
async fn connect_with_busy_retry(
    connector: &ExampleLocalIpcConnector,
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
pub async fn local_ipc_server_service(
    listener: Arc<ExampleLocalIpcListener>,
) -> anyhow::Result<()> {
    service_daemon::done();

    {
        // `LocalIpcConnect` opens one initialization probe connection and then
        // closes its client end. This block accepts only that probe; the server
        // side closes naturally at the end of the block without reading data.
        let _probe_connection = match listener.accept().await {
            Ok(connection) => connection,
            Err(source) => {
                return Err(LocalIpcError::AcceptInitializationProbe {
                    name: EXAMPLE_LOCAL_IPC_NAME,
                    source,
                }
                .into());
            }
        };
        info!(
            name = EXAMPLE_LOCAL_IPC_NAME,
            "Accepted and closed the local IPC connector initialization probe"
        );
    }

    let mut business_connection = match listener.accept().await {
        Ok(connection) => connection,
        Err(source) => {
            return Err(LocalIpcError::AcceptBusinessConnection {
                name: EXAMPLE_LOCAL_IPC_NAME,
                source,
            }
            .into());
        }
    };
    let request = match read_request(&mut business_connection).await {
        Ok(request) => request,
        Err(source) => {
            return Err(LocalIpcError::ReadRequest {
                name: EXAMPLE_LOCAL_IPC_NAME,
                source,
            }
            .into());
        }
    };
    info!(
        request = %String::from_utf8_lossy(&request),
        "Received local IPC request"
    );

    if let Err(source) = write_response(&mut business_connection).await {
        return Err(LocalIpcError::WriteResponse {
            name: EXAMPLE_LOCAL_IPC_NAME,
            source,
        }
        .into());
    }
    info!("Sent local IPC response");

    service_daemon::wait_shutdown().await;
    Ok(())
}

#[service]
pub async fn local_ipc_client_service(
    connector: Arc<ExampleLocalIpcConnector>,
) -> anyhow::Result<()> {
    #[cfg(unix)]
    let connection = connector.connect().await;
    #[cfg(windows)]
    let connection = connect_with_busy_retry(&connector).await;

    let mut connection = match connection {
        Ok(connection) => connection,
        Err(source) => {
            return Err(LocalIpcError::ConnectClient {
                name: EXAMPLE_LOCAL_IPC_NAME,
                source,
            }
            .into());
        }
    };

    if let Err(source) = write_request(&mut connection).await {
        return Err(LocalIpcError::WriteRequest {
            name: EXAMPLE_LOCAL_IPC_NAME,
            source,
        }
        .into());
    }

    let response = match read_response(&mut connection).await {
        Ok(response) => response,
        Err(source) => {
            return Err(LocalIpcError::ReadResponse {
                name: EXAMPLE_LOCAL_IPC_NAME,
                source,
            }
            .into());
        }
    };
    info!(
        response = %String::from_utf8_lossy(&response),
        "Received local IPC response"
    );

    service_daemon::done();
    service_daemon::wait_shutdown().await;
    Ok(())
}
