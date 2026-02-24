//! # Simulation Example — Unit Testing with MockContext
//!
//! This example demonstrates the `simulation` feature:
//! - `MockContext::builder()` for creating isolated test environments
//! - `with_mock::<T>()` for injecting shadow Provider values
//! - `with_service_name()` for shelf namespace isolation
//! - `with_log_drain()` for capturing logs in test output
//!
//! The `simulation` feature is compile-time gated, ensuring zero overhead
//! in production builds.
//!
//! **Run tests**: `cargo test -p example-simulation`

/// This file is intentionally minimal — the real demonstration is in the tests below.
fn main() {
    tracing_subscriber::fmt::init();
    tracing::info!("This example is designed to be run as tests:");
    tracing::info!("  cargo test -p example-simulation");
}

// =============================================================================
// Integration Tests — Simulation / MockContext
// =============================================================================
#[cfg(test)]
mod tests {
    use service_daemon::provider;
    use service_daemon::{MockContext, Provided};

    // --- Mock Provider Setup ---

    /// A config provider we want to mock in tests.
    #[derive(Clone, Debug, PartialEq)]
    #[provider(default = "production-db://real-host")]
    pub struct DatabaseUrl(pub String);

    /// A port provider we want to mock in tests.
    #[derive(Clone, Debug, PartialEq)]
    #[provider(default = 5432)]
    pub struct Port(pub i32);

    // --- Tests ---

    /// Demonstrates injecting a mock Provider value and verifying
    /// that `resolve()` returns the shadow value instead of the default.
    #[tokio::test]
    async fn test_mock_provider_injection() {
        let ctx = MockContext::builder()
            .with_service_name("mock_test")
            .with_mock::<DatabaseUrl>(DatabaseUrl("test-db://localhost".into()))
            .with_mock::<Port>(Port(9999))
            .with_log_drain()
            .build();

        ctx.run(|| async {
            // Inside the mock scope, resolve() returns shadow values
            let db_url = DatabaseUrl::resolve().await;
            let port = Port::resolve().await;

            assert_eq!(db_url.0, "test-db://localhost");
            assert_eq!(port.0, 9999);
        })
        .await;
    }

    /// Demonstrates that `shelve()` / `unshelve()` work within MockContext
    /// using isolated shelf storage.
    #[tokio::test]
    async fn test_shelf_isolation_in_mock() {
        let ctx = MockContext::builder()
            .with_service_name("shelf_test")
            .with_log_drain()
            .build();

        ctx.run(|| async {
            // Shelve data within the mock scope
            service_daemon::shelve("counter", 42u32).await;

            // Unshelve within the same scope
            let value = service_daemon::unshelve::<u32>("counter").await;
            assert_eq!(value, Some(42));
        })
        .await;
    }

    /// Demonstrates that `state()` returns `Initializing` by default
    /// in a fresh MockContext.
    #[tokio::test]
    async fn test_default_state_in_mock() {
        let ctx = MockContext::builder()
            .with_service_name("state_test")
            .build();

        ctx.run(|| async {
            let status = service_daemon::state();
            assert!(
                matches!(status, service_daemon::ServiceStatus::Initializing),
                "Expected Initializing, got {:?}",
                status
            );
        })
        .await;
    }

    /// Demonstrates that two MockContexts are fully isolated from each other.
    #[tokio::test]
    async fn test_mock_context_isolation() {
        let ctx_a = MockContext::builder()
            .with_service_name("service_a")
            .with_mock::<DatabaseUrl>(DatabaseUrl("db_a".into()))
            .build();

        let ctx_b = MockContext::builder()
            .with_service_name("service_b")
            .with_mock::<DatabaseUrl>(DatabaseUrl("db_b".into()))
            .build();

        let result_a = ctx_a
            .run(|| async {
                let url = DatabaseUrl::resolve().await;
                url.0.clone()
            })
            .await;

        let result_b = ctx_b
            .run(|| async {
                let url = DatabaseUrl::resolve().await;
                url.0.clone()
            })
            .await;

        assert_eq!(result_a, "db_a");
        assert_eq!(result_b, "db_b");
    }
}
