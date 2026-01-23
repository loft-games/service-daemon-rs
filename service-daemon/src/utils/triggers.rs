use futures::future::BoxFuture;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

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

/// Generic host for signal-based triggers.
pub async fn signal_trigger_host<F>(
    name: &str,
    notifier: Arc<tokio::sync::Notify>,
    handler: F,
    cancellation_token: CancellationToken,
) -> anyhow::Result<()>
where
    F: Fn(String) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync + 'static,
{
    loop {
        tokio::select! {
            _ = notifier.notified() => {
                let trigger_id = generate_trigger_id();
                info!("Signal trigger '{}' fired (ID: {})", name, trigger_id);
                if let Err(e) = handler(trigger_id).await {
                    error!("Trigger '{}' error: {:?}", name, e);
                }
            }
            _ = cancellation_token.cancelled() => {
                info!("Signal trigger '{}' shutting down", name);
                break;
            }
        }
    }
    Ok(())
}

/// Generic host for broadcast queue triggers.
pub async fn queue_trigger_host<T, F>(
    name: &str,
    mut receiver: tokio::sync::broadcast::Receiver<T>,
    handler: F,
    cancellation_token: CancellationToken,
) -> anyhow::Result<()>
where
    T: Clone + Send + Sync + 'static,
    F: Fn(T, String) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync + 'static,
{
    loop {
        tokio::select! {
            res = receiver.recv() => {
                match res {
                    Ok(value) => {
                        let trigger_id = generate_trigger_id();
                        info!("Queue trigger '{}' received item (ID: {})", name, trigger_id);
                        if let Err(e) = handler(value, trigger_id).await {
                            error!("Trigger '{}' error: {:?}", name, e);
                        }
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
            _ = cancellation_token.cancelled() => {
                info!("Queue trigger '{}' shutting down", name);
                break;
            }
        }
    }
    Ok(())
}

/// Generic host for load-balancing queue triggers.
pub async fn lb_queue_trigger_host<T, F>(
    name: &str,
    receiver_mutex: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<T>>>,
    handler: F,
    cancellation_token: CancellationToken,
) -> anyhow::Result<()>
where
    T: Send + Sync + 'static,
    F: Fn(T, String) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync + 'static,
{
    loop {
        let item = tokio::select! {
            item = async {
                let mut receiver = receiver_mutex.lock().await;
                receiver.recv().await
            } => item,
            _ = cancellation_token.cancelled() => {
                info!("LB Queue trigger '{}' shutting down", name);
                break;
            }
        };

        match item {
            Some(value) => {
                let trigger_id = generate_trigger_id();
                info!(
                    "LB Queue trigger '{}' received item (ID: {})",
                    name, trigger_id
                );
                if let Err(e) = handler(value, trigger_id).await {
                    error!("Trigger '{}' error: {:?}", name, e);
                }
            }
            None => {
                warn!("LB Queue trigger '{}' channel closed", name);
                break;
            }
        }
    }
    Ok(())
}

/// Generic host for cron triggers.
#[cfg(feature = "cron")]
pub async fn cron_trigger_host<F>(
    name: &str,
    schedule: &str,
    handler: F,
    cancellation_token: CancellationToken,
) -> anyhow::Result<()>
where
    F: Fn(String) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync + 'static,
{
    use tokio_cron_scheduler::{Job, JobScheduler};

    let mut sched = JobScheduler::new().await?;
    let handler = Arc::new(handler);
    let name_str = name.to_string();

    let job = Job::new_async(schedule, move |_uuid, _lock| {
        let trigger_id = generate_trigger_id();
        let h = handler.clone();
        let n = name_str.clone();
        Box::pin(async move {
            info!("Cron trigger '{}' fired (ID: {})", n, trigger_id);
            if let Err(e) = h(trigger_id).await {
                error!("Trigger '{}' error: {:?}", n, e);
            }
        })
    })?;

    sched.add(job).await?;
    sched.start().await?;

    tokio::select! {
        _ = cancellation_token.cancelled() => {
            info!("Cron trigger '{}' shutting down", name);
            let _ = sched.shutdown().await;
        }
    }

    Ok(())
}
