use std::io::{Write as _, stderr};
use std::sync::Arc;

use tokio::sync::broadcast;

use crate::ServicePriority;
use crate::service;

use super::model::{LogEvent, effective_batch_size, get_log_queue};
use super::render::{LogProcessingGuard, render_to_buf};

/// A background service that consumes the LogQueue and renders events to stderr.
/// It uses ShutdownOrder::SYSTEM (100) to ensure it exits last.
///
/// ## Responsibility
/// Console output **only**. File persistence is handled by the independent
/// `file_log_service` (behind the `file-logging` feature gate).
///
/// ## Batch Drain Strategy
/// Instead of processing events one-by-one with per-event lock acquisition,
/// this service uses a batch buffer:
/// 1. Block until at least one event arrives (`recv().await`).
/// 2. Greedily drain all immediately available events via `try_recv()`.
/// 3. Flush the entire batch in one pass with a single reentrancy guard.
#[service(priority = ServicePriority::SYSTEM, tags = ["__log__"])]
pub async fn log_service() -> anyhow::Result<()> {
    let mut rx = get_log_queue().tx.subscribe();
    let batch_size = effective_batch_size();
    let mut buffer: Vec<Arc<LogEvent>> = Vec::with_capacity(batch_size);

    while !service_daemon::is_shutdown() {
        tokio::select! {
            biased;
            _ = service_daemon::wait_shutdown() => {
                break;
            }
            result = rx.recv() => {
                match result {
                    Ok(event) => {
                        buffer.push(event);

                        // Greedily drain all immediately available events
                        while buffer.len() < batch_size {
                            match rx.try_recv() {
                                Ok(event) => buffer.push(event),
                                Err(_) => break,
                            }
                        }

                        // Flush the entire batch under a single reentrancy guard
                        {
                            let _guard = LogProcessingGuard::enter();
                            let mut render_buf = String::with_capacity(256);
                            for event in buffer.drain(..) {
                                render_to_buf(&event, &mut render_buf);
                                render_buf.push('\n');
                                {
                                    let stderr = stderr();
                                    let _ = stderr.lock().write_all(render_buf.as_bytes());
                                }
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "LogService lagged, some messages were dropped");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    // Drain any remaining logs before exiting
    while let Ok(event) = rx.try_recv() {
        buffer.push(event);
    }
    if !buffer.is_empty() {
        let _guard = LogProcessingGuard::enter();
        let mut render_buf = String::with_capacity(256);
        for event in buffer.drain(..) {
            render_to_buf(&event, &mut render_buf);
            render_buf.push('\n');
            {
                let stderr = stderr();
                let _ = stderr.lock().write_all(render_buf.as_bytes());
            }
        }
    }

    tracing::info!("LogService shutting down (Priority: SYSTEM)");
    Ok(())
}
