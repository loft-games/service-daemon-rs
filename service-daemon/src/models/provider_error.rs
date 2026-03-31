use thiserror::Error;

/// Provider initialization error.
///
/// This error type is handled by the framework runtime (retry/backoff/exit)
/// rather than being returned to user code.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProviderError {
    /// Non-recoverable provider failure.
    ///
    /// The daemon should fail-fast.
    #[error("Fatal provider error: {0}")]
    Fatal(String),
    /// Recoverable provider failure.
    ///
    /// The daemon should retry with backoff until `RestartPolicy::provider_init_timeout`
    /// is exceeded.
    #[error("Retryable provider error: {0}")]
    Retryable(String),
}
