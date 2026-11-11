#![allow(dead_code)]
use service_daemon::__private::futures::{FutureExt, future::BoxFuture};

use service_daemon::{
    DaemonInstanceHandle, DaemonInstanceId, Registry, ServiceDaemon, ServiceInstanceId, service,
    service_handle,
};
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, LazyLock, Mutex, Weak};

type DriverJobs = HashMap<DaemonInstanceId, BoxFuture<'static, ()>>;
static JOBS: LazyLock<Mutex<DriverJobs>> = LazyLock::new(|| Mutex::new(HashMap::new()));

type PublishedValues = HashMap<(ServiceInstanceId, TypeId), Weak<dyn Any + Send + Sync>>;
static PUBLISHED: LazyLock<Mutex<PublishedValues>> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn publish<T: Send + Sync + 'static>(value: &Arc<T>) {
    let instance = service_daemon::current_service_instance_id();
    let erased: Arc<dyn Any + Send + Sync> = value.clone();
    let mut published = PUBLISHED.lock().unwrap();
    published.retain(|_, value| value.strong_count() > 0);
    published.insert((instance, TypeId::of::<T>()), Arc::downgrade(&erased));
}

pub async fn published<T: Send + Sync + 'static>(daemon: &DaemonInstanceHandle) -> Arc<T> {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            for instance in daemon.service_instances() {
                let value = PUBLISHED
                    .lock()
                    .unwrap()
                    .get(&(instance.instance_id(), TypeId::of::<T>()))
                    .and_then(Weak::upgrade);
                if let Some(value) = value {
                    return value
                        .downcast::<T>()
                        .unwrap_or_else(|_| panic!("published type must match"));
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "daemon service did not publish {}",
            std::any::type_name::<T>()
        )
    })
}

#[service(tags = ["__provider_test_context"])]
async fn provider_test_driver() -> anyhow::Result<()> {
    let owner = service_handle!(provider_test_driver)
        .map_err(|error| anyhow::anyhow!("{error:?}"))?
        .daemon_id();
    let job = JOBS.lock().unwrap().remove(&owner);
    service_daemon::done();
    if let Some(job) = job {
        job.await;
    }
    service_daemon::wait_shutdown().await;
    Ok(())
}

pub async fn run<F>(future: F) -> F::Output
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__provider_test_context")
                .build(),
        )
        .build();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    JOBS.lock().unwrap().insert(
        daemon.id(),
        Box::pin(async move {
            let result = AssertUnwindSafe(future).catch_unwind().await;
            let _ = sender.send(result);
        }),
    );
    daemon.run().await;
    let result = receiver.await;
    daemon.shutdown();
    daemon
        .wait()
        .await
        .expect("provider test daemon should stop");
    match result.expect("provider test driver should return its result") {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}
