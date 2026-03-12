/// Provider initialization error.
///
/// This error type is handled by the framework runtime (retry/backoff/exit)
/// rather than being returned to user code.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// Non-recoverable provider failure.
    ///
    /// The daemon should fail-fast.
    Fatal(String),
    /// Recoverable provider failure.
    ///
    /// The daemon should retry with backoff until `RestartPolicy::provider_init_timeout`
    /// is exceeded.
    Retryable(String),
}
