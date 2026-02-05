use thiserror::Error;

/// Core error type for the Service Daemon framework.
#[derive(Debug, Error)]
pub enum ServiceError {
    /// Failed to initialize a service.
    #[error("Failed to initialize service '{0}': {1}")]
    StartupError(String, String),

    /// A required dependency is missing or failed to resolve.
    #[error("Dependency resolution failed for '{0}': {1}")]
    DependencyMissing(String, String),

    /// Error in the service registry or linkme collection.
    #[error("Registry error: {0}")]
    RegistryError(String),

    /// Service failed to stop within the grace period.
    #[error("Service '{0}' timed out during shutdown")]
    ShutdownTimeout(String),

    /// An internal task or channel error.
    #[error("Internal error: {0}")]
    InternalError(String),

    /// A fatal error that should permanently stop the service without restart.
    /// Use this for unrecoverable errors (e.g., invalid configuration, license issues).
    #[error("Fatal error in service: {0}")]
    Fatal(String),
}

/// A specialized Result type for Service Daemon operations.
pub type Result<T> = std::result::Result<T, ServiceError>;
