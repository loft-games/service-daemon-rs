use futures::future::BoxFuture;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, error, info, warn};

#[cfg(feature = "uuid-trigger-ids")]
use uuid::Uuid;

/// Helper to generate a unique trigger ID if the feature is enabled.
fn generate_trigger_id() -> String {
    #[cfg(feature = "uuid-trigger-ids")]
    {
        Uuid::new_v4().to_string()
    }
    #[cfg(not(feature = "uuid-trigger-ids"))]
    {
        "static-id".to_string()
    }
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
    while !crate::utils::context::is_shutdown() {
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
    while !crate::utils::context::is_shutdown() {
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
        }
    }
    Ok(())
}

pub async fn lb_queue_trigger_host<T, F>(
    name: &str,
    receiver_mutex: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<T>>>,
    handler: F,
    _cancellation_token: CancellationToken,
) -> anyhow::Result<()>
where
    T: Send + Sync + 'static,
    F: Fn(T) -> BoxFuture<'static, anyhow::Result<()>> + Clone + Send + Sync + 'static,
{
    while !crate::utils::context::is_shutdown() {
        let item = async {
            let mut receiver = receiver_mutex.lock().await;
            receiver.recv().await
        }
        .await;

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
    use tokio_cron_scheduler::{Job, JobScheduler};

    let mut sched = JobScheduler::new().await?;
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

    sched.add(job).await?;
    sched.start().await?;

    // For cron, we start the scheduler and just wait.
    // Cron scheduler manages its own tasks.
    while !crate::utils::context::is_shutdown() {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let _ = sched.shutdown().await;

    Ok(())
}

pub async fn watch_trigger_host<T, F>(
    name: &str,
    handler: F,
    _cancellation_token: CancellationToken,
) -> anyhow::Result<()>
where
    T: crate::utils::di::Provided + Send + Sync + 'static,
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
    crate::utils::context::wait_shutdown().await;
    Ok(())
}
