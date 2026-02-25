use futures::future::BoxFuture;
use std::sync::Arc;
use tokio::sync::{Mutex, OnceCell};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, error, info, warn};

use crate::core::context;

/// Helper to generate a unique trigger ID if the feature is enabled.
fn generate_trigger_id() -> String {
    #[cfg(feature = "uuid-trigger-ids")]
    {
        uuid::Uuid::new_v4().to_string()
    }
    #[cfg(not(feature = "uuid-trigger-ids"))]
    {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("trigger-{}", id)
    }
}

/// Global shared scheduler for all cron triggers.
/// Using tokio::sync::OnceCell for async-native initialization.
static SHARED_SCHEDULER: OnceCell<tokio_cron_scheduler::JobScheduler> = OnceCell::const_new();

async fn get_shared_scheduler() -> anyhow::Result<tokio_cron_scheduler::JobScheduler> {
    // OnceCell::get_or_try_init is async and non-blocking
    SHARED_SCHEDULER
        .get_or_try_init(|| async {
            let sched = tokio_cron_scheduler::JobScheduler::new().await?;
            sched.start().await?;
            Ok::<_, anyhow::Error>(sched)
        })
        .await
        .cloned()
}

pub async fn signal_trigger_host<F>(
    name: &str,
    notifier: Arc<tokio::sync::Notify>,
    handler: F,
    _cancellation_token: CancellationToken,
) -> anyhow::Result<()>
where
    F: Fn() -> BoxFuture<'static, anyhow::Result<()>> + Clone + Send + Sync + 'static,
{
    while !context::is_shutdown() {
        tokio::select! {
            _ = notifier.notified() => {
                let id = generate_trigger_id();
                let span = tracing::info_span!("trigger", %name, %id);
                let h = handler.clone();
                async move {
                    info!("Signal trigger fired");
                    if let Err(e) = h().await {
                        error!("Trigger error: {:?}", e);
                    }
                }.instrument(span).await;
            }
            _ = context::wait_shutdown() => {
                info!("Signal trigger '{}' received shutdown, exiting", name);
                break;
            }
        }
    }
    Ok(())
}

pub async fn queue_trigger_host<T, F>(
    name: &str,
    mut receiver: tokio::sync::broadcast::Receiver<T>,
    handler: F,
    _cancellation_token: CancellationToken,
) -> anyhow::Result<()>
where
    T: Clone + Send + Sync + 'static,
    F: Fn(T) -> BoxFuture<'static, anyhow::Result<()>> + Clone + Send + Sync + 'static,
{
    while !context::is_shutdown() {
        tokio::select! {
            res = receiver.recv() => {
                match res {
                    Ok(value) => {
                        let id = generate_trigger_id();
                        let span = tracing::info_span!("trigger", %name, %id);
                        let h = handler.clone();
                        async move {
                            info!("Queue trigger received item");
                            if let Err(e) = h(value).await {
                                error!("Trigger error: {:?}", e);
                            }
                        }.instrument(span).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Queue trigger '{}' lagged by {} messages", name, n);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        warn!("Queue trigger '{}' channel closed", name);
                        break;
                    }
                }
            }
            _ = context::wait_shutdown() => {
                info!("Queue trigger '{}' received shutdown, exiting", name);
                break;
            }
        }
    }
    Ok(())
}

pub async fn lb_queue_trigger_host<T, F>(
    name: &str,
    receiver_mutex: Arc<Mutex<tokio::sync::mpsc::Receiver<T>>>,
    handler: F,
    _cancellation_token: CancellationToken,
) -> anyhow::Result<()>
where
    T: Send + Sync + 'static,
    F: Fn(T) -> BoxFuture<'static, anyhow::Result<()>> + Clone + Send + Sync + 'static,
{
    while !context::is_shutdown() {
        // We need to select between receiving an item and shutdown
        let item = tokio::select! {
            result = async {
                let mut receiver = receiver_mutex.lock().await;
                receiver.recv().await
            } => result,
            _ = context::wait_shutdown() => {
                info!("LB Queue trigger '{}' received shutdown, exiting", name);
                return Ok(());
            }
        };

        match item {
            Some(value) => {
                let id = generate_trigger_id();
                let span = tracing::info_span!("trigger", %name, %id);
                let h = handler.clone();
                async move {
                    info!("LB Queue trigger received item");
                    if let Err(e) = h(value).await {
                        error!("Trigger error: {:?}", e);
                    }
                }
                .instrument(span)
                .await;
            }
            None => {
                warn!("LB Queue trigger '{}' channel closed", name);
                break;
            }
        }
    }
    Ok(())
}

pub async fn cron_trigger_host<F>(
    name: &str,
    schedule: &str,
    handler: F,
    _cancellation_token: CancellationToken,
) -> anyhow::Result<()>
where
    F: Fn() -> BoxFuture<'static, anyhow::Result<()>> + Clone + Send + Sync + 'static,
{
    use tokio_cron_scheduler::Job;

    let sched = get_shared_scheduler().await?;
    let handler = Arc::new(handler);
    let name_str = name.to_string();

    let job = Job::new_async(schedule, move |_uuid, _lock| {
        let id = generate_trigger_id();
        let h = handler.clone();
        let n = name_str.clone();
        Box::pin(async move {
            let span = tracing::info_span!("trigger", name = %n, %id);
            async move {
                info!("Cron trigger fired");
                if let Err(e) = h().await {
                    error!("Trigger error: {:?}", e);
                }
            }
            .instrument(span)
            .await;
        })
    })?;

    let job_id = sched.add(job).await?;

    // For cron, we wait for shutdown.
    // The shared scheduler manages the execution in the background.
    context::wait_shutdown().await;

    // Remove the job from the shared scheduler before exiting
    if let Err(e) = sched.remove(&job_id).await {
        error!(
            "Failed to remove cron job '{}' from shared scheduler: {:?}",
            name, e
        );
    } else {
        info!("Removed cron job '{}' from shared scheduler", name);
    }

    Ok(())
}

pub async fn watch_trigger_host<T, F>(
    name: &str,
    handler: F,
    _cancellation_token: CancellationToken,
) -> anyhow::Result<()>
where
    T: crate::core::di::Provided + Send + Sync + 'static,
    F: Fn(Arc<T>) -> BoxFuture<'static, anyhow::Result<()>> + Clone + Send + Sync + 'static,
{
    // Resolve a fresh snapshot to pass to the handler
    let snapshot = T::resolve().await;
    let id = generate_trigger_id();
    let span = tracing::info_span!("trigger", %name, %id);
    let h = handler.clone();

    async move {
        info!("Watch trigger fired (instance started)");
        if let Err(e) = h(snapshot).await {
            error!("Trigger error: {:?}", e);
        }
    }
    .instrument(span)
    .await;

    // Wait for the next reload (triggered by dependency watcher) or shutdown
    context::wait_shutdown().await;
    Ok(())
}
