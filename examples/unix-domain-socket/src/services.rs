use crate::providers::{
    EXAMPLE_UNIX_DOMAIN_SOCKET_PATH, ExampleUnixDomainSocketConnector,
    ExampleUnixDomainSocketListener,
};

use crate::models::error::UnixDomainSocketError;
use service_daemon::{ServicePriority, service};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::info;

const REQUEST_PAYLOAD: &[u8] = b"hello";
const RESPONSE_PAYLOAD: &[u8] = b"world";

#[service(priority = ServicePriority::STORAGE)]
pub async fn unix_domain_socket_server_service(
    listener: Arc<ExampleUnixDomainSocketListener>,
) -> anyhow::Result<()> {
    service_daemon::done();

    let listener = match listener.get() {
        Ok(listener) => listener,
        Err(source) => {
            return Err(UnixDomainSocketError::GetListener {
                path: EXAMPLE_UNIX_DOMAIN_SOCKET_PATH,
                source,
            }
            .into());
        }
    };
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

    {
        // `UnixConnect` opens one initialization probe connection and then
        // closes its client end. This block accepts only that probe; the server
        // side closes naturally at the end of the block without reading data.
        let (_probe_connection, _) = match listener.accept().await {
            Ok(connection) => connection,
            Err(source) => {
                return Err(UnixDomainSocketError::AcceptInitializationProbe {
                    path: EXAMPLE_UNIX_DOMAIN_SOCKET_PATH,
                    source,
                }
                .into());
            }
        };
        info!(
            path = EXAMPLE_UNIX_DOMAIN_SOCKET_PATH,
            "Accepted and closed the connector initialization probe"
        );
    }

    let (mut business_connection, _) = match listener.accept().await {
        Ok(connection) => connection,
        Err(source) => {
            return Err(UnixDomainSocketError::AcceptBusinessConnection {
                path: EXAMPLE_UNIX_DOMAIN_SOCKET_PATH,
                source,
            }
            .into());
        }
    };
    let mut request = [0_u8; REQUEST_PAYLOAD.len()];
    if let Err(source) = business_connection.read_exact(&mut request).await {
        return Err(UnixDomainSocketError::ReadRequest {
            path: EXAMPLE_UNIX_DOMAIN_SOCKET_PATH,
            source,
        }
        .into());
    }
    info!(
        request = %String::from_utf8_lossy(&request),
        "Received Unix domain socket request"
    );

    if let Err(source) = business_connection.write_all(RESPONSE_PAYLOAD).await {
        return Err(UnixDomainSocketError::WriteResponse {
            path: EXAMPLE_UNIX_DOMAIN_SOCKET_PATH,
            source,
        }
        .into());
    }
    info!("Sent Unix domain socket response");

    service_daemon::wait_shutdown().await;
    Ok(())
}

#[service]
pub async fn unix_domain_socket_client_service(
    connector: Arc<ExampleUnixDomainSocketConnector>,
) -> anyhow::Result<()> {
    let mut connection = match connector.connect().await {
        Ok(connection) => connection,
        Err(source) => {
            return Err(UnixDomainSocketError::ConnectClient {
                path: EXAMPLE_UNIX_DOMAIN_SOCKET_PATH,
                source,
            }
            .into());
        }
    };
    if let Err(source) = connection.write_all(REQUEST_PAYLOAD).await {
        return Err(UnixDomainSocketError::WriteRequest {
            path: EXAMPLE_UNIX_DOMAIN_SOCKET_PATH,
            source,
        }
        .into());
    }

    let mut response = [0_u8; RESPONSE_PAYLOAD.len()];
    if let Err(source) = connection.read_exact(&mut response).await {
        return Err(UnixDomainSocketError::ReadResponse {
            path: EXAMPLE_UNIX_DOMAIN_SOCKET_PATH,
            source,
        }
        .into());
    }
    info!(
        response = %String::from_utf8_lossy(&response),
        "Received Unix domain socket response"
    );

    service_daemon::done();
    service_daemon::wait_shutdown().await;
    Ok(())
}
