//! Error models for the cross-platform local IPC example.

use std::{fmt, io};

#[derive(Debug)]
pub enum LocalIpcError {
    AcceptInitializationProbe {
        name: &'static str,
        source: io::Error,
    },
    AcceptBusinessConnection {
        name: &'static str,
        source: io::Error,
    },
    ReadRequest {
        name: &'static str,
        source: io::Error,
    },
    UnexpectedRequest {
        name: &'static str,
        expected: &'static [u8],
        actual: Vec<u8>,
    },
    WriteResponse {
        name: &'static str,
        source: io::Error,
    },
    ConnectClient {
        name: &'static str,
        source: io::Error,
    },
    WriteRequest {
        name: &'static str,
        source: io::Error,
    },
    ReadResponse {
        name: &'static str,
        source: io::Error,
    },
    UnexpectedResponse {
        name: &'static str,
        expected: &'static [u8],
        actual: Vec<u8>,
    },
}

impl fmt::Display for LocalIpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AcceptInitializationProbe { name, .. } => {
                write!(
                    f,
                    "failed to accept local IPC connector initialization probe for {name}"
                )
            }
            Self::AcceptBusinessConnection { name, .. } => {
                write!(
                    f,
                    "failed to accept local IPC business connection for {name}"
                )
            }
            Self::ReadRequest { name, .. } => {
                write!(f, "failed to read local IPC request for {name}")
            }
            Self::UnexpectedRequest {
                name,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "unexpected local IPC request for {name}: expected {expected:?}, got {actual:?}"
                )
            }
            Self::WriteResponse { name, .. } => {
                write!(f, "failed to write local IPC response for {name}")
            }
            Self::ConnectClient { name, .. } => {
                write!(f, "failed to connect to local IPC server for {name}")
            }
            Self::WriteRequest { name, .. } => {
                write!(f, "failed to write local IPC request for {name}")
            }
            Self::ReadResponse { name, .. } => {
                write!(f, "failed to read local IPC response for {name}")
            }
            Self::UnexpectedResponse {
                name,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "unexpected local IPC response for {name}: expected {expected:?}, got {actual:?}"
                )
            }
        }
    }
}

impl std::error::Error for LocalIpcError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AcceptInitializationProbe { source, .. }
            | Self::AcceptBusinessConnection { source, .. }
            | Self::ReadRequest { source, .. }
            | Self::WriteResponse { source, .. }
            | Self::ConnectClient { source, .. }
            | Self::WriteRequest { source, .. }
            | Self::ReadResponse { source, .. } => Some(source),
            Self::UnexpectedRequest { .. } | Self::UnexpectedResponse { .. } => None,
        }
    }
}
