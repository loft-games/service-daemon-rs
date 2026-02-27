use chrono::{DateTime, Utc};
#[cfg(feature = "file-logging")]
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{OnceCell, broadcast};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

/// Represents a captured log event with structured metadata.
///
/// This is the canonical log event format used throughout the daemon.
/// When the `file-logging` feature is enabled, events are serialized to JSON
/// and persisted to disk with automatic rotation.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "file-logging", derive(Serialize, Deserialize))]
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
        // Fallback for sync contexts. Safe because LogQueue::default() is non-async.
        // If set() fails (another thread raced us), get() still succeeds because the
        // other thread's value is already stored. This is guaranteed by OnceCell semantics.
        let _ = LOG_QUEUE.set(LogQueue::default());
        LOG_QUEUE
            .get()
            .expect("OnceCell invariant violated: set() succeeded but get() returned None")
    })
}

/// Configuration for file-based log persistence.
///
/// Controls the output directory, file prefix, and rotation strategy.
/// Only available when the `file-logging` feature is enabled.
///
/// # Rotation Strategy
/// Uses daily rotation by default. Log files are named with the pattern:
/// `{prefix}.YYYY-MM-DD` and stored in the configured directory.
///
/// # Example
/// ```rust,ignore
/// use service_daemon::core::logging::FileLogConfig;
///
/// let config = FileLogConfig::new("logs", "app");
/// ```
#[cfg(feature = "file-logging")]
#[derive(Debug, Clone)]
pub struct FileLogConfig {
    /// Directory where log files are stored (e.g., "logs").
    pub directory: String,
    /// File name prefix (e.g., "app" produces "app.2026-02-24").
    pub file_prefix: String,
}

#[cfg(feature = "file-logging")]
impl FileLogConfig {
    /// Creates a new file log configuration.
    ///
    /// # Arguments
    /// * `directory` - Path to the log output directory (created if missing).
    /// * `file_prefix` - Prefix for rotated log file names.
    #[must_use]
    pub fn new(directory: impl Into<String>, file_prefix: impl Into<String>) -> Self {
        Self {
            directory: directory.into(),
            file_prefix: file_prefix.into(),
        }
    }
}

#[cfg(feature = "file-logging")]
impl Default for FileLogConfig {
    fn default() -> Self {
        Self {
            directory: "logs".to_string(),
            file_prefix: "daemon".to_string(),
        }
    }
}

/// Global file log configuration, set once before the daemon starts.
/// When `None`, file logging is disabled even if the feature is compiled in.
#[cfg(feature = "file-logging")]
static FILE_LOG_CONFIG: OnceCell<FileLogConfig> = OnceCell::const_new();

/// Enables file-based log persistence with the given configuration.
///
/// Must be called **before** `ServiceDaemon::run()` to take effect.
/// If not called, the `log_service` will only output to stderr (console).
///
/// # Arguments
/// * `config` - File logging configuration specifying directory and prefix.
///
/// # Example
/// ```rust,ignore
/// use service_daemon::core::logging::{FileLogConfig, enable_file_logging};
///
/// enable_file_logging(FileLogConfig::new("logs", "my-app"));
/// ```
#[cfg(feature = "file-logging")]
pub fn enable_file_logging(config: FileLogConfig) {
    let _ = FILE_LOG_CONFIG.set(config);
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

/// Formats a `LogEvent` as a structured JSON string for file persistence.
///
/// Output follows IGES 6.8: includes `level`, `time` (ISO 8601), `target`,
/// `msg`, `caller` (file:line), and `module_path`.
#[cfg(feature = "file-logging")]
fn format_event_json(event: &LogEvent) -> String {
    // Primary path: use serde_json for correct, structured JSON output.
    // Fallback: manual format! if serialization unexpectedly fails.
    serde_json::to_string(event).unwrap_or_else(|_| {
        format!(
            r#"{{"level":"{}","time":"{}","target":"{}","msg":"{}"}}"#,
            event.level,
            event.timestamp.to_rfc3339(),
            event.target,
            event.message
        )
    })
}

/// A background service that consumes the LogQueue and handles final output.
/// It uses ShutdownOrder::SYSTEM (100) to ensure it exits last.
///
/// ## Behavior
/// - **Always**: Outputs log events to stderr in human-readable format.
/// - **With `file-logging` + `enable_file_logging()`**: Additionally persists
///   events as JSON lines to a daily-rotating log file.
#[service_daemon::service(priority = service_daemon::ServicePriority::SYSTEM)]
pub async fn log_service() -> anyhow::Result<()> {
    let mut rx = get_log_queue().tx.subscribe();

    // Initialize file writer if file-logging is enabled and configured.
    #[cfg(feature = "file-logging")]
    let mut file_writer = FILE_LOG_CONFIG.get().map(|config| {
        let file_appender =
            tracing_appender::rolling::daily(&config.directory, &config.file_prefix);
        // non_blocking returns a guard that must be held alive for the writer to work.
        let (writer, guard) = tracing_appender::non_blocking(file_appender);
        (writer, guard)
    });

    #[cfg(feature = "file-logging")]
    let has_file_output = file_writer.is_some();

    while !service_daemon::is_shutdown() {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(event) => {
                        // Console output (always active) -- human-readable format
                        // Uses stderr to avoid infinite recursion if tracing subscriber
                        // is also watching stdout.
                        eprintln!(
                            "[{}] {:<5} [{}] {}",
                            event.timestamp.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
                            event.level,
                            event.target,
                            event.message
                        );

                        // File output (only when file-logging feature is enabled and configured)
                        #[cfg(feature = "file-logging")]
                        if has_file_output
                            && let Some((ref mut writer, _)) = file_writer
                        {
                            use std::io::Write;
                            let json_line = format_event_json(&event);
                            let _ = writeln!(writer, "{}", json_line);
                        }
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

        #[cfg(feature = "file-logging")]
        if let Some((ref mut writer, _)) = file_writer {
            use std::io::Write;
            let json_line = format_event_json(&event);
            let _ = writeln!(writer, "{}", json_line);
        }
    }

    eprintln!("LogService shutting down (Priority: SYSTEM)");
    Ok(())
}

/// Subscribes to the internal log queue, returning a broadcast receiver.
///
/// This is used by `MockContext` (under the `simulation` feature) to drain
/// log events that would otherwise be silently discarded when `LogService`
/// is not running in unit test environments.
#[cfg(feature = "simulation")]
pub fn subscribe_log_queue() -> tokio::sync::broadcast::Receiver<Arc<LogEvent>> {
    get_log_queue().tx.subscribe()
}
