use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{OnceCell, broadcast};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

/// Represents a captured log event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEvent {
    pub timestamp: DateTime<Utc>,
    pub level: String,
    pub target: String,
    pub message: String,
    pub module_path: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
}

/// A high-performance broadcast queue for log events.
/// This allows multiple consumers (like LogService and potentially a DevConsole).
pub struct LogQueue {
    pub tx: broadcast::Sender<Arc<LogEvent>>,
}

impl Default for LogQueue {
    fn default() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self { tx }
    }
}

/// Using tokio::sync::OnceCell for async-native initialization.
static LOG_QUEUE: OnceCell<LogQueue> = OnceCell::const_new();

/// Gets the log queue, initializing it if necessary.
/// This is safe to call from both sync and async contexts due to OnceCell's design.
fn get_log_queue() -> &'static LogQueue {
    // For the tracing layer (sync context), we use blocking_get or initialize synchronously.
    // OnceCell::get() returns Option, get_or_init requires async.
    // Since this is called from a sync tracing layer, we use try_get or a sync fallback.
    // The LOG_QUEUE will be initialized on first use in either context.
    LOG_QUEUE.get().unwrap_or_else(|| {
        // This is a fallback for sync contexts. Initialize synchronously.
        // This is safe because LogQueue::default() doesn't require async.
        let _ = LOG_QUEUE.set(LogQueue::default());
        LOG_QUEUE.get().expect("LogQueue should be initialized")
    })
}

/// A non-blocking tracing Layer that captures events and pushes them to the LogQueue.
pub struct DaemonLayer;

impl<S> Layer<S> for DaemonLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();

        // Simple message extraction (can be expanded to handle fields)
        let mut message = String::new();
        struct MessageVisitor<'a>(&'a mut String);
        impl<'a> tracing::field::Visit for MessageVisitor<'a> {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    use std::fmt::Write;
                    let _ = write!(self.0, "{:?}", value);
                }
            }
        }
        event.record(&mut MessageVisitor(&mut message));

        let log_event = Arc::new(LogEvent {
            timestamp: Utc::now(),
            level: metadata.level().to_string(),
            target: metadata.target().to_string(),
            message,
            module_path: metadata.module_path().map(|s| s.to_string()),
            file: metadata.file().map(|s| s.to_string()),
            line: metadata.line(),
        });

        // Non-blocking send
        let _ = get_log_queue().tx.send(log_event);
    }
}

/// A background service that consumes the LogQueue and handles final output.
/// It uses ShutdownOrder::SYSTEM (100) to ensure it exits last.
#[service_daemon::service(priority = service_daemon::ServicePriority::SYSTEM)]
pub async fn log_service() -> anyhow::Result<()> {
    let mut rx = get_log_queue().tx.subscribe();

    while !service_daemon::is_shutdown() {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(event) => {
                        // In a real application, you might write to a file or a remote collector here.
                        // For now, we'll output to standard error to avoid infinitely recursive tracing
                        // if the standard subscriber is also looking at stdout.
                        eprintln!(
                            "[{}] {:<5} [{}] {}",
                            event.timestamp.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
                            event.level,
                            event.target,
                            event.message
                        );
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        eprintln!("LogService lagged by {} messages", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = service_daemon::wait_shutdown() => {
                break;
            }
        }
    }

    // Drain any remaining logs before exiting
    while let Ok(event) = rx.try_recv() {
        eprintln!(
            "[{}] {:<5} [{}] {} (Drained)",
            event.timestamp.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
            event.level,
            event.target,
            event.message
        );
    }

    eprintln!("LogService shutting down (Priority: SYSTEM)");
    Ok(())
}
