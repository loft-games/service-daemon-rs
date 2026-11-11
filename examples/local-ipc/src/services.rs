//! Services that exercise the cross-platform local IPC provider templates.

use crate::models::error::LocalIpcError;
use crate::providers::{EXAMPLE_LOCAL_IPC_NAME, ExampleLocalIpcConnector, ExampleLocalIpcListener};
use bytes::{Bytes, BytesMut};
use futures_util::{SinkExt, StreamExt};
use service_daemon::{ServicePriority, service};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use tracing::info;

#[service(priority = ServicePriority::STORAGE)]
pub async fn local_ipc_server_service(
    listener: Arc<ExampleLocalIpcListener>,
) -> anyhow::Result<()> {
    service_daemon::done();

    let business_connection = match listener.accept().await {
        Ok(connection) => connection,
        Err(source) => {
            return Err(LocalIpcError::AcceptBusinessConnection {
                name: EXAMPLE_LOCAL_IPC_NAME,
                source,
            }
            .into());
        }
    };
    let mut connection = Framed::new(business_connection, LengthDelimitedCodec::new());
    let request = read_frame(&mut connection, |source| LocalIpcError::ReadRequest {
        name: EXAMPLE_LOCAL_IPC_NAME,
        source,
    })
    .await?;
    info!(
        request = %String::from_utf8_lossy(&request),
        "Received local IPC request"
    );

    write_frame(&mut connection, b"pong", |source| {
        LocalIpcError::WriteResponse {
            name: EXAMPLE_LOCAL_IPC_NAME,
            source,
        }
    })
    .await?;
    info!("Sent local IPC response");

    service_daemon::wait_shutdown().await;
    Ok(())
}

#[service]
pub async fn local_ipc_client_service(
    connector: Arc<ExampleLocalIpcConnector>,
) -> anyhow::Result<()> {
    let connection = connector.connect().await;

    let connection = match connection {
        Ok(connection) => connection,
        Err(source) => {
            return Err(LocalIpcError::ConnectClient {
                name: EXAMPLE_LOCAL_IPC_NAME,
                source,
            }
            .into());
        }
    };
    let mut connection = Framed::new(connection, LengthDelimitedCodec::new());

    write_frame(&mut connection, b"ping", |source| {
        LocalIpcError::WriteRequest {
            name: EXAMPLE_LOCAL_IPC_NAME,
            source,
        }
    })
    .await?;

    let response = read_frame(&mut connection, |source| LocalIpcError::ReadResponse {
        name: EXAMPLE_LOCAL_IPC_NAME,
        source,
    })
    .await?;
    info!(
        response = %String::from_utf8_lossy(&response),
        "Received local IPC response"
    );

    service_daemon::done();
    service_daemon::wait_shutdown().await;
    Ok(())
}

async fn read_frame<S>(
    connection: &mut Framed<S, LengthDelimitedCodec>,
    error: impl Fn(std::io::Error) -> LocalIpcError,
) -> Result<BytesMut, LocalIpcError>
where
    S: AsyncRead + Unpin,
{
    match connection.next().await {
        Some(Ok(frame)) => Ok(frame),
        Some(Err(source)) => Err(error(source)),
        None => Err(error(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "connection closed",
        ))),
    }
}

async fn write_frame<S>(
    connection: &mut Framed<S, LengthDelimitedCodec>,
    payload: &'static [u8],
    error: impl FnOnce(std::io::Error) -> LocalIpcError,
) -> Result<(), LocalIpcError>
where
    S: AsyncWrite + Unpin,
{
    connection
        .send(Bytes::copy_from_slice(payload))
        .await
        .map_err(error)
}
