use crate::providers::{
    EXAMPLE_UNIX_DOMAIN_SOCKET_PATH, ExampleUnixDomainSocketConnector,
    ExampleUnixDomainSocketListener,
};

use crate::models::error::UnixDomainSocketError;
use bytes::{Bytes, BytesMut};
use futures_util::{SinkExt, StreamExt};
use service_daemon::{ServicePriority, service};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use tracing::info;

#[service(priority = ServicePriority::STORAGE)]
pub async fn unix_domain_socket_server_service(
    listener: Arc<ExampleUnixDomainSocketListener>,
) -> anyhow::Result<()> {
    service_daemon::done();

    let local_addr = match listener.local_addr() {
        Ok(addr) => addr,
        Err(source) => {
            return Err(UnixDomainSocketError::ReadListenerLocalAddress {
                path: EXAMPLE_UNIX_DOMAIN_SOCKET_PATH,
                source,
            }
            .into());
        }
    };
    info!(
        path = EXAMPLE_UNIX_DOMAIN_SOCKET_PATH,
        addr = ?local_addr,
        "Unix domain socket server listening"
    );

    let business_connection = match listener.accept().await {
        Ok(connection) => connection,
        Err(source) => {
            return Err(UnixDomainSocketError::AcceptBusinessConnection {
                path: EXAMPLE_UNIX_DOMAIN_SOCKET_PATH,
                source,
            }
            .into());
        }
    };
    let mut connection = Framed::new(business_connection, LengthDelimitedCodec::new());
    let request = read_frame(&mut connection, |source| {
        UnixDomainSocketError::ReadRequest {
            path: EXAMPLE_UNIX_DOMAIN_SOCKET_PATH,
            source,
        }
    })
    .await?;
    info!(
        request = %String::from_utf8_lossy(&request),
        "Received Unix domain socket request"
    );

    write_frame(&mut connection, b"pong", |source| {
        UnixDomainSocketError::WriteResponse {
            path: EXAMPLE_UNIX_DOMAIN_SOCKET_PATH,
            source,
        }
    })
    .await?;
    info!("Sent Unix domain socket response");

    service_daemon::wait_shutdown().await;
    Ok(())
}

#[service]
pub async fn unix_domain_socket_client_service(
    connector: Arc<ExampleUnixDomainSocketConnector>,
) -> anyhow::Result<()> {
    let connection = match connector.connect().await {
        Ok(connection) => connection,
        Err(source) => {
            return Err(UnixDomainSocketError::ConnectClient {
                path: EXAMPLE_UNIX_DOMAIN_SOCKET_PATH,
                source,
            }
            .into());
        }
    };
    let mut connection = Framed::new(connection, LengthDelimitedCodec::new());
    write_frame(&mut connection, b"ping", |source| {
        UnixDomainSocketError::WriteRequest {
            path: EXAMPLE_UNIX_DOMAIN_SOCKET_PATH,
            source,
        }
    })
    .await?;

    let response = read_frame(&mut connection, |source| {
        UnixDomainSocketError::ReadResponse {
            path: EXAMPLE_UNIX_DOMAIN_SOCKET_PATH,
            source,
        }
    })
    .await?;
    info!(
        response = %String::from_utf8_lossy(&response),
        "Received Unix domain socket response"
    );

    service_daemon::done();
    service_daemon::wait_shutdown().await;
    Ok(())
}

async fn read_frame<S>(
    connection: &mut Framed<S, LengthDelimitedCodec>,
    error: impl Fn(std::io::Error) -> UnixDomainSocketError,
) -> Result<BytesMut, UnixDomainSocketError>
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
    error: impl FnOnce(std::io::Error) -> UnixDomainSocketError,
) -> Result<(), UnixDomainSocketError>
where
    S: AsyncWrite + Unpin,
{
    connection
        .send(Bytes::copy_from_slice(payload))
        .await
        .map_err(error)
}
