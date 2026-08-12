//! Error models for the Windows named pipe example.

use std::{fmt, io};

#[derive(Debug)]
pub enum NamedPipeError {
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

impl fmt::Display for NamedPipeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AcceptInitializationProbe { name, .. } => {
                write!(
                    f,
                    "failed to accept named pipe connector initialization probe at {name}"
                )
            }
            Self::AcceptBusinessConnection { name, .. } => {
                write!(
                    f,
                    "failed to accept named pipe business connection at {name}"
                )
            }
            Self::ReadRequest { name, .. } => {
                write!(f, "failed to read named pipe request at {name}")
            }
            Self::UnexpectedRequest {
                name,
                expected,
                actual,
            } => write!(
                f,
                "unexpected named pipe request at {name}: expected {expected:?}, got {actual:?}"
            ),
            Self::WriteResponse { name, .. } => {
                write!(f, "failed to write named pipe response at {name}")
            }
            Self::ConnectClient { name, .. } => {
                write!(f, "failed to connect to named pipe server at {name}")
            }
            Self::WriteRequest { name, .. } => {
                write!(f, "failed to write named pipe request at {name}")
            }
            Self::ReadResponse { name, .. } => {
                write!(f, "failed to read named pipe response at {name}")
            }
            Self::UnexpectedResponse {
                name,
                expected,
                actual,
            } => write!(
                f,
                "unexpected named pipe response at {name}: expected {expected:?}, got {actual:?}"
            ),
        }
    }
}

impl std::error::Error for NamedPipeError {
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
