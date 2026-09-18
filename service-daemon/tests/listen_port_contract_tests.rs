use service_daemon::{ProviderError, provider};
use std::{net::Ipv4Addr, process::Command, time::Duration};

#[provider(Listen(0))]
struct NumericListener;
#[provider(Listen("0000"))]
struct StringListener;
#[provider(Listen("127.0.0.1:0"))]
struct LoopbackListener;
#[provider(Listen(0), env = "SD_LISTEN_PORT_CONTRACT", eager = true)]
#[derive(Debug)]
struct EnvListener;
#[provider(Listen("65536"))]
#[derive(Debug)]
struct InvalidListener;

#[tokio::test]
async fn literals_bind_and_accept_connections() {
    let numeric = NumericListener::try_new().unwrap();
    let string = StringListener::try_new().unwrap();
    let loopback = LoopbackListener::try_new().unwrap();
    for (listener, expected) in [
        (numeric.get().unwrap(), Ipv4Addr::UNSPECIFIED),
        (string.get().unwrap(), Ipv4Addr::UNSPECIFIED),
        (loopback.get().unwrap(), Ipv4Addr::LOCALHOST),
    ] {
        let addr = listener.local_addr().unwrap();
        assert_eq!(addr.ip(), expected);
        assert_ne!(addr.port(), 0);
        tokio::time::timeout(Duration::from_secs(5), async {
            let client = tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, addr.port()))
                .await
                .unwrap();
            let (_, peer) = listener.accept().await.unwrap();
            assert_eq!(peer, client.local_addr().unwrap());
        })
        .await
        .unwrap();
    }
    assert!(matches!(
        InvalidListener::try_new(),
        Err(ProviderError::Fatal(_))
    ));
}

#[tokio::test]
async fn env_child() {
    let Ok(expected) = std::env::var("SD_LISTEN_EXPECTED") else {
        return;
    };
    let result = <EnvListener as service_daemon::ManagedProvided>::resolve_managed().await;
    if expected == "invalid" {
        assert!(matches!(result, Err(ProviderError::Fatal(_))), "{result:?}");
    } else if expected == "conflict" {
        assert!(
            matches!(result, Err(ProviderError::Retryable(_))),
            "{result:?}"
        );
    } else {
        let listener = result.unwrap();
        let addr = listener.get().unwrap().local_addr().unwrap();
        assert_eq!(addr.ip().to_string(), expected);
        assert_ne!(addr.port(), 0);
    }
}

#[test]
fn environment_port_and_address_matrix() {
    for (value, expected) in [
        (None, "0.0.0.0"),
        (Some("0"), "0.0.0.0"),
        (Some(" 0000 \t"), "0.0.0.0"),
        (Some("127.0.0.1:0"), "127.0.0.1"),
        (Some(" 127.0.0.1:0 "), "127.0.0.1"),
        (Some("65536"), "invalid"),
        (Some("999999999999999999999999"), "invalid"),
        (Some("-1"), "invalid"),
        (Some("1.5"), "invalid"),
        (Some(""), "invalid"),
        (Some(" \t"), "invalid"),
        (Some("invalid-address"), "invalid"),
    ] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "env_child", "--nocapture"])
            .env("SD_LISTEN_EXPECTED", expected)
            .env_remove("SD_LISTEN_PORT_CONTRACT");
        if let Some(value) = value {
            child.env("SD_LISTEN_PORT_CONTRACT", value);
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{value:?}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn environment_nonzero_port_preserves_bind_conflict_classification() {
    // Keep the socket alive: the child must attempt this exact port, not port 0.
    let occupied = std::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
    let port = occupied.local_addr().unwrap().port();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "env_child", "--nocapture"])
        .env("SD_LISTEN_EXPECTED", "conflict")
        .env("SD_LISTEN_PORT_CONTRACT", port.to_string())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
