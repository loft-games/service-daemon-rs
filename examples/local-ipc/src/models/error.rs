//! Error models for the cross-platform local IPC example.

use std::{fmt, io};

#[derive(Debug)]
pub enum LocalIpcError {
    AcceptBusinessConnection {
        name: &'static str,
        source: io::Error,
    },
    ReadRequest {
        name: &'static str,
        source: io::Error,
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
}

impl fmt::Display for LocalIpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AcceptBusinessConnection { name, .. } => {
                write!(
                    f,
                    "failed to accept local IPC business connection for {name}"
                )
            }
            Self::ReadRequest { name, .. } => {
                write!(f, "failed to read local IPC request for {name}")
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
        }
    }
}

impl std::error::Error for LocalIpcError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AcceptBusinessConnection { source, .. }
            | Self::ReadRequest { source, .. }
            | Self::WriteResponse { source, .. }
            | Self::ConnectClient { source, .. }
            | Self::WriteRequest { source, .. }
            | Self::ReadResponse { source, .. } => Some(source),
        }
    }
}
