use service_daemon::ServiceDaemon;
use service_daemon_macro::provider;
use std::sync::atomic::{AtomicBool, Ordering};

static EAGER_INIT_CALLED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Default)]
pub struct EagerToken(pub String);

#[provider(eager = true)]
async fn eager_provider() -> EagerToken {
    EAGER_INIT_CALLED.store(true, Ordering::SeqCst);
    EagerToken("eager".to_string())
}

#[service_daemon::service(tags = ["stub_for_eager_test"])]
async fn stub_service(_token: std::sync::Arc<EagerToken>) -> anyhow::Result<()> {
    Ok(())
}

#[tokio::test]
async fn test_async_fn_eager_init() {
    // Reset state for test
    EAGER_INIT_CALLED.store(false, Ordering::SeqCst);

    let mut daemon = ServiceDaemon::builder()
        .with_registry(
            service_daemon::models::Registry::builder()
                .with_tag("stub_for_eager_test")
                .build(),
        )
        .build();

    // Before run, eager provider should NOT be initialized
    assert!(!EAGER_INIT_CALLED.load(Ordering::SeqCst));

    // ServiceDaemon::run initializes eager providers synchronously in our case
    // until it hits the event loop.
    let _ = daemon.run().await;

    assert!(EAGER_INIT_CALLED.load(Ordering::SeqCst));
}
