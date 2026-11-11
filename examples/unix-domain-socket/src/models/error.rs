//! Error models for the Unix domain socket example.

use std::{fmt, io};

#[derive(Debug)]
pub enum UnixDomainSocketError {
    ReadListenerLocalAddress {
        path: &'static str,
        source: io::Error,
    },
    AcceptBusinessConnection {
        path: &'static str,
        source: io::Error,
    },
    ReadRequest {
        path: &'static str,
        source: io::Error,
    },
    WriteResponse {
        path: &'static str,
        source: io::Error,
    },
    ConnectClient {
        path: &'static str,
        source: io::Error,
    },
    WriteRequest {
        path: &'static str,
        source: io::Error,
    },
    ReadResponse {
        path: &'static str,
        source: io::Error,
    },
}

impl fmt::Display for UnixDomainSocketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadListenerLocalAddress { path, .. } => {
                write!(f, "failed to read Unix listener local address at {path}")
            }
            Self::AcceptBusinessConnection { path, .. } => {
                write!(f, "failed to accept Unix business connection at {path}")
            }
            Self::ReadRequest { path, .. } => {
                write!(f, "failed to read Unix socket request at {path}")
            }
            Self::WriteResponse { path, .. } => {
                write!(f, "failed to write Unix socket response at {path}")
            }
            Self::ConnectClient { path, .. } => {
                write!(f, "failed to connect to Unix socket server at {path}")
            }
            Self::WriteRequest { path, .. } => {
                write!(f, "failed to write Unix socket request at {path}")
            }
            Self::ReadResponse { path, .. } => {
                write!(f, "failed to read Unix socket response at {path}")
            }
        }
    }
}

impl std::error::Error for UnixDomainSocketError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ReadListenerLocalAddress { source, .. }
            | Self::AcceptBusinessConnection { source, .. }
            | Self::ReadRequest { source, .. }
            | Self::WriteResponse { source, .. }
            | Self::ConnectClient { source, .. }
            | Self::WriteRequest { source, .. }
            | Self::ReadResponse { source, .. } => Some(source),
        }
    }
}
