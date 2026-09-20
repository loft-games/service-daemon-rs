use futures::FutureExt;
use service_daemon::{ManagedProvided, Provided, WatchableProvided, provider};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};

#[path = "support/provider_context.rs"]
mod provider_context;

static INITIALIZATIONS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Debug)]
struct ContextRequired;

#[provider]
fn context_required() -> ContextRequired {
    INITIALIZATIONS.fetch_add(1, Ordering::SeqCst);
    ContextRequired
}

#[tokio::test]
async fn missing_context_rejects_all_resolution_before_initialization() {
    let snapshot = <ContextRequired as Provided>::resolve().await.unwrap_err();
    assert!(
        snapshot
            .to_string()
            .contains("provider resolution requires a daemon context")
    );
    assert!(snapshot.to_string().contains("ContextRequired"));
    assert!(
        <ContextRequired as ManagedProvided>::resolve_rwlock()
            .await
            .is_err()
    );
    assert!(
        <ContextRequired as ManagedProvided>::resolve_mutex()
            .await
            .is_err()
    );
    assert!(ContextRequired::resolve_managed().await.is_err());
    assert!(
        AssertUnwindSafe(ContextRequired::resolve())
            .catch_unwind()
            .await
            .is_err()
    );
    assert!(
        AssertUnwindSafe(ContextRequired::resolve_rwlock())
            .catch_unwind()
            .await
            .is_err()
    );
    assert!(
        AssertUnwindSafe(ContextRequired::resolve_mutex())
            .catch_unwind()
            .await
            .is_err()
    );
    assert!(std::panic::catch_unwind(ContextRequired::watch_dependency).is_err());
    assert_eq!(INITIALIZATIONS.load(Ordering::SeqCst), 0);
}

#[derive(Clone, Debug)]
struct IndependentValue;

#[provider]
fn independent_value() -> IndependentValue {
    IndependentValue
}

#[tokio::test]
async fn concurrent_daemons_isolate_instances_and_release_them_after_external_drop() {
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let second_barrier = barrier.clone();
    let (first, second) = tokio::join!(
        provider_context::run(async move {
            let value = IndependentValue::resolve().await;
            barrier.wait().await;
            value
        }),
        provider_context::run(async move {
            let value = IndependentValue::resolve().await;
            second_barrier.wait().await;
            value
        }),
    );
    assert!(!std::sync::Arc::ptr_eq(&first, &second));
    let first_weak = std::sync::Arc::downgrade(&first);
    let second_weak = std::sync::Arc::downgrade(&second);
    drop(first);
    assert!(first_weak.upgrade().is_none());
    assert!(second_weak.upgrade().is_some());
    drop(second);
    assert!(second_weak.upgrade().is_none());
}

static RESTART_INITS: AtomicUsize = AtomicUsize::new(0);
static RESTART_RUNS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone)]
struct RestartValue(usize);

#[provider]
fn restart_value() -> RestartValue {
    RestartValue(RESTART_INITS.fetch_add(1, Ordering::SeqCst) + 1)
}

#[service_daemon::service(tags = ["__provider_restart_identity"])]
async fn restart_consumer(value: std::sync::Arc<RestartValue>) -> anyhow::Result<()> {
    assert_eq!(value.0, 1);
    assert!(std::sync::Arc::ptr_eq(
        &value,
        &RestartValue::resolve().await
    ));
    let generation = RESTART_RUNS.fetch_add(1, Ordering::SeqCst);
    service_daemon::done();
    if generation == 0 {
        anyhow::bail!("exercise ordinary provider-preserving restart");
    }
    service_daemon::wait_shutdown().await;
    Ok(())
}

#[tokio::test]
async fn ordinary_generation_restart_reuses_daemon_provider() {
    let daemon = service_daemon::ServiceDaemon::builder()
        .with_registry(
            service_daemon::Registry::builder()
                .with_tag("__provider_restart_identity")
                .build(),
        )
        .with_restart_policy(service_daemon::RestartPolicy::for_testing())
        .build();
    daemon.run().await;
    let restarted = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while RESTART_RUNS.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await;
    daemon.shutdown();
    daemon.wait().await.unwrap();
    assert!(restarted.is_ok());
    assert_eq!(RESTART_INITS.load(Ordering::SeqCst), 1);
}
