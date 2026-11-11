//! Services that exercise the Windows named pipe provider templates.

use crate::models::error::NamedPipeError;
use crate::providers::{
    EXAMPLE_NAMED_PIPE_NAME, ExampleNamedPipeConnector, ExampleNamedPipeListener,
};
use bytes::{Bytes, BytesMut};
use futures_util::{SinkExt, StreamExt};
use service_daemon::{ServicePriority, service};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use tracing::info;

#[service(priority = ServicePriority::STORAGE)]
pub async fn named_pipe_server_service(
    listener: Arc<ExampleNamedPipeListener>,
) -> anyhow::Result<()> {
    service_daemon::done();

    let business_connection = match listener.accept().await {
        Ok(connection) => connection,
        Err(source) => {
            return Err(NamedPipeError::AcceptBusinessConnection {
                name: EXAMPLE_NAMED_PIPE_NAME,
                source,
            }
            .into());
        }
    };
    let mut connection = Framed::new(business_connection, LengthDelimitedCodec::new());
    let request = read_frame(&mut connection, |source| NamedPipeError::ReadRequest {
        name: EXAMPLE_NAMED_PIPE_NAME,
        source,
    })
    .await?;
    info!(
        request = %String::from_utf8_lossy(&request),
        "Received Windows named pipe request"
    );

    write_frame(&mut connection, b"pong", |source| {
        NamedPipeError::WriteResponse {
            name: EXAMPLE_NAMED_PIPE_NAME,
            source,
        }
    })
    .await?;
    info!("Sent Windows named pipe response");

    service_daemon::wait_shutdown().await;
    Ok(())
}

#[service]
pub async fn named_pipe_client_service(
    connector: Arc<ExampleNamedPipeConnector>,
) -> anyhow::Result<()> {
    let connection = match connector.connect().await {
        Ok(connection) => connection,
        Err(source) => {
            return Err(NamedPipeError::ConnectClient {
                name: EXAMPLE_NAMED_PIPE_NAME,
                source,
            }
            .into());
        }
    };
    let mut connection = Framed::new(connection, LengthDelimitedCodec::new());
    write_frame(&mut connection, b"ping", |source| {
        NamedPipeError::WriteRequest {
            name: EXAMPLE_NAMED_PIPE_NAME,
            source,
        }
    })
    .await?;

    let response = read_frame(&mut connection, |source| NamedPipeError::ReadResponse {
        name: EXAMPLE_NAMED_PIPE_NAME,
        source,
    })
    .await?;
    info!(
        response = %String::from_utf8_lossy(&response),
        "Received Windows named pipe response"
    );

    service_daemon::done();
    service_daemon::wait_shutdown().await;
    Ok(())
}

async fn read_frame<S>(
    connection: &mut Framed<S, LengthDelimitedCodec>,
    error: impl Fn(std::io::Error) -> NamedPipeError,
) -> Result<BytesMut, NamedPipeError>
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
    error: impl FnOnce(std::io::Error) -> NamedPipeError,
) -> Result<(), NamedPipeError>
where
    S: AsyncWrite + Unpin,
{
    connection
        .send(Bytes::copy_from_slice(payload))
        .await
        .map_err(error)
}
