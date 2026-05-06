use service_daemon::{Registry, RestartPolicy, ServiceDaemon, ServiceStatus, provider, service};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;

static RELOAD_EVENTS: LazyLock<Mutex<Vec<&'static str>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static RELOAD_GENERATIONS: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Default)]
#[provider]
pub struct ReloadConfig {
    pub version: u32,
}

#[service(tags = ["__test_reload_contract__"])]
async fn reload_observer(_config: Arc<ReloadConfig>) -> anyhow::Result<()> {
    RELOAD_GENERATIONS.fetch_add(1, Ordering::SeqCst);

    loop {
        match service_daemon::state() {
            ServiceStatus::Initializing => {
                RELOAD_EVENTS.lock().await.push("initializing");
                service_daemon::done();
            }
            ServiceStatus::Restoring => {
                RELOAD_EVENTS.lock().await.push("restoring");
                service_daemon::done();
            }
            ServiceStatus::Healthy => {
                if !service_daemon::sleep(Duration::from_millis(20)).await {
                    continue;
                }
            }
            ServiceStatus::NeedReload => {
                RELOAD_EVENTS.lock().await.push("need_reload");
                service_daemon::done();
                break;
            }
            ServiceStatus::ShuttingDown => break,
            _ => break,
        }
    }

    Ok(())
}

#[tokio::test]
async fn test_dependency_reload_transitions_through_need_reload_and_restoring() -> anyhow::Result<()>
{
    RELOAD_GENERATIONS.store(0, Ordering::SeqCst);
    RELOAD_EVENTS.lock().await.clear();

    let mut daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__test_reload_contract__")
                .build(),
        )
        .with_restart_policy(RestartPolicy::for_testing())
        .build();

    let cancel = daemon.cancel_token();
    daemon.run().await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    {
        let lock = <ReloadConfig as service_daemon::ManagedProvided>::resolve_rwlock()
            .await
            .expect("reload config should resolve");
        let mut guard = lock.write().await;
        guard.version += 1;
    }

    tokio::time::sleep(Duration::from_millis(400)).await;
    cancel.cancel();
    daemon.wait().await?;

    let events = RELOAD_EVENTS.lock().await.clone();
    assert!(
        events.contains(&"initializing"),
        "initial generation not observed: {:?}",
        events
    );
    assert!(
        events.contains(&"need_reload"),
        "NeedReload was not observed after dependency mutation: {:?}",
        events
    );
    assert!(
        events.contains(&"restoring"),
        "restoring generation not observed after reload: {:?}",
        events
    );
    assert!(
        RELOAD_GENERATIONS.load(Ordering::SeqCst) >= 2,
        "expected at least two generations after reload"
    );

    Ok(())
}
