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
    /// This provider implementation cannot provide the requested value in the current process.
    ///
    /// Ordinary `#[provider]` declarations have no fallback candidate, so the
    /// daemon treats this as fatal. `#[provider_impl]` candidates for a
    /// `#[provider_contract]` output use it to advance to the next registered
    /// candidate.
    Unavailable(String),
}
