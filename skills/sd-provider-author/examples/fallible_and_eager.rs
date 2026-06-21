// Fallible async-function provider with correct ProviderError classification.
use service_daemon::{ProviderError, provider};
use std::sync::Arc;

// Dependencies are injected as Arc<T>. Return the plain value `T`; the framework
// wraps it in Arc<T>. Classify failures:
//   - Retryable: transient, the daemon retries with backoff until
//     RestartPolicy::provider_init_timeout.
//   - Fatal: unrecoverable, the daemon fails fast and shuts down.
#[provider]
pub async fn db_pool(url: Arc<DbUrl>) -> Result<DatabasePool, ProviderError> {
    match DatabasePool::connect(&url).await {
        Ok(pool) => Ok(pool),
        Err(e) if e.is_transient() => Err(ProviderError::Retryable(format!("DB not ready: {e}"))),
        Err(e) => Err(ProviderError::Fatal(format!("DB config invalid: {e}"))),
    }
}

// Eager: run at startup so a failure aborts the daemon before dependents start.
// Eager applies only to providers reachable from the selected services.
#[provider(eager = true)]
pub async fn run_migrations(pool: Arc<DatabasePool>) -> Result<(), ProviderError> {
    pool.migrate()
        .await
        .map_err(|e| ProviderError::Fatal(format!("migration failed: {e}")))
}
