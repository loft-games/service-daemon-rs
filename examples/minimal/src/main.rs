//! # Minimal Example -- `is_shutdown()` Polling Pattern
//!
//! This is the simplest way to use `service-daemon`. It demonstrates:
//! - Defining a service with `#[service]`
//! - Using `is_shutdown()` for graceful exit
//! - Basic dependency injection via `#[provider]`
//! - The interruptible `sleep()` helper
//!
//! **Run**: `cargo run -p example-minimal`
//!
//! > [!WARNING]
//! > Do NOT mix `is_shutdown()` polling with `state()` lifecycle matching
//! > in the same service. These are two independent control-flow paradigms;
//! > mixing them leads to undefined behavior.

mod providers;
mod services;

use service_daemon::ServiceDaemon;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Initialize tracing with the built-in DaemonLayer for log capture.
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(service_daemon::core::logging::DaemonLayer)
        .init();

    // 2. Create a daemon from the global service registry.
    //    All `#[service]`-annotated functions in this crate are auto-registered.
    let mut daemon = ServiceDaemon::builder().build();

    // 3. Start the daemon (non-blocking).
    daemon.run().await;

    // 4. Wait for shutdown signal (Ctrl+C / SIGTERM),
    //    then perform ordered, graceful shutdown.
    daemon.wait().await?;

    Ok(())
}

// =============================================================================
// Integration Tests -- Minimal
// =============================================================================
#[cfg(test)]
mod tests {
    use service_daemon::{Registry, RestartPolicy, ServiceDaemon};

    /// Verifies that a minimal daemon can start and stop cleanly
    /// without any complex lifecycle management.
    #[tokio::test]
    async fn test_minimal_startup_and_shutdown() -> anyhow::Result<()> {
        let mut daemon = ServiceDaemon::builder()
            .with_registry(Registry::builder().with_tag("__test_isolation__").build())
            .with_restart_policy(RestartPolicy::for_testing())
            .build();
        let cancel = daemon.cancel_token();

        daemon.run().await;

        // Allow services to initialize
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Trigger graceful shutdown
        cancel.cancel();
        daemon.wait().await.unwrap();

        Ok(())
    }

    /// Verifies that `is_shutdown()` becomes true after cancellation,
    /// allowing services to exit their polling loops.
    #[tokio::test]
    async fn test_is_shutdown_responsiveness() -> anyhow::Result<()> {
        use service_daemon::tokio_util::sync::CancellationToken;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let exited = Arc::new(AtomicBool::new(false));
        let exited_clone = exited.clone();

        let shutdown_fn: service_daemon::ServiceFn = Arc::new(move |_| {
            let flag = exited_clone.clone();
            Box::pin(async move {
                service_daemon::done();
                while !service_daemon::is_shutdown() {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                flag.store(true, Ordering::SeqCst);
                Ok(())
            })
        });

        let mut daemon = ServiceDaemon::builder()
            .with_registry(Registry::builder().with_tag("__test_isolation__").build())
            .with_restart_policy(RestartPolicy::for_testing())
            .with_service(service_daemon::ServiceDescription {
                id: service_daemon::ServiceId::new(0),
                name: "shutdown_test".to_string(),
                run: shutdown_fn,
                watcher: None,
                priority: 50,
                cancellation_token: CancellationToken::new(),
                tags: vec![],
            })
            .build();

        let cancel = daemon.cancel_token();

        daemon.run().await;

        // Wait for service to reach Healthy
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(!exited.load(Ordering::SeqCst), "Service exited prematurely");

        cancel.cancel();
        daemon.wait().await.unwrap();

        assert!(
            exited.load(Ordering::SeqCst),
            "Service did not exit after shutdown signal"
        );
        Ok(())
    }
}
