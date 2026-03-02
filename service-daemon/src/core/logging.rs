use chrono::{DateTime, Utc};
#[cfg(feature = "file-logging")]
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::sync::{Arc, OnceLock};
use tokio::sync::broadcast;
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

/// Global log queue, initialized on first access.
/// Uses `std::sync::OnceLock` instead of `tokio::sync::OnceCell` because
/// `LogQueue::default()` is synchronous (just `broadcast::channel`), and
/// `OnceLock::get_or_init` provides race-free initialization without the
/// theoretical double-init window that `OnceCell::get() + set()` has.
static LOG_QUEUE: OnceLock<LogQueue> = OnceLock::new();

/// Gets the log queue, initializing it on first call.
fn get_log_queue() -> &'static LogQueue {
    LOG_QUEUE.get_or_init(LogQueue::default)
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
static FILE_LOG_CONFIG: tokio::sync::OnceCell<FileLogConfig> = tokio::sync::OnceCell::const_new();

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

// ---------------------------------------------------------------------------
// Reentrancy guard: prevents infinite recursion when log_service emits
// tracing events during log processing.
//
// Mechanism: `tracing::info!()` triggers `DaemonLayer::on_event()` synchronously
// on the SAME OS thread. A thread-local flag detects this reentrancy. When the
// guard is active, events bypass the LogQueue and are written directly to stderr.
// ---------------------------------------------------------------------------

thread_local! {
    /// Thread-local flag set to `true` while `log_service` is processing a log event.
    /// Checked by `DaemonLayer::on_event()` to prevent recursive queue insertion.
    static IN_LOG_PROCESSING: Cell<bool> = const { Cell::new(false) };
}

/// RAII guard that marks the current thread as "inside log processing".
/// On drop (including panic unwinding), the flag is automatically cleared.
struct LogProcessingGuard;

impl LogProcessingGuard {
    /// Activates the reentrancy guard for the current thread.
    fn enter() -> Self {
        IN_LOG_PROCESSING.with(|f| f.set(true));
        LogProcessingGuard
    }
}

impl Drop for LogProcessingGuard {
    fn drop(&mut self) {
        IN_LOG_PROCESSING.with(|f| f.set(false));
    }
}

// ---------------------------------------------------------------------------
// Field collection: extracts message and structured fields from tracing events.
// ---------------------------------------------------------------------------

/// Collects the message and structured fields from a tracing event.
///
/// Implements dual-path capture:
/// - `record_str`: called for `&str` values, produces clean output without Debug quotes.
/// - `record_debug`: fallback for `fmt::Arguments`, `u64`, `bool`, etc.
///   `fmt::Arguments::Debug` delegates to `Display` (no extra quotes).
struct FieldCollector {
    message: String,
    fields: Vec<(String, String)>,
}

impl FieldCollector {
    fn new() -> Self {
        Self {
            message: String::new(),
            fields: Vec::new(),
        }
    }

    /// Builds the final message string.
    /// If structured fields are present, appends them as `{ key=value, ... }`.
    fn into_message(self) -> String {
        if self.fields.is_empty() {
            self.message
        } else {
            let pairs: Vec<String> = self
                .fields
                .iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect();
            format!("{} {{ {} }}", self.message, pairs.join(", "))
        }
    }
}

impl tracing::field::Visit for FieldCollector {
    /// Priority path for `&str` values. Avoids Debug quote wrapping on the message field.
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }
    }

    /// Fallback path for non-string types (`fmt::Arguments`, `u64`, `bool`, etc.).
    /// `fmt::Arguments::Debug` delegates to `Display`, so no extra quotes are added
    /// for formatted messages like `tracing::info!("port = {}", 80)`.
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let formatted = format!("{:?}", value);
        if field.name() == "message" {
            self.message = formatted;
        } else {
            self.fields.push((field.name().to_string(), formatted));
        }
    }
}

/// Formats a log event to stderr in human-readable format.
/// Shared by both the normal log_service output path and the reentrancy fallback.
fn format_to_stderr(event: &LogEvent) {
    eprintln!(
        "[{}] {:<5} [{}] {}",
        event.timestamp.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
        event.level,
        event.target,
        event.message
    );
}

/// A non-blocking tracing Layer that captures events and pushes them to the LogQueue.
///
/// When reentrancy is detected (i.e., `log_service` emits a tracing event while
/// processing a log), the event bypasses the queue and is written directly to stderr
/// to prevent infinite recursion.
pub struct DaemonLayer;

impl<S> Layer<S> for DaemonLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();

        // Collect message and structured fields from the event
        let mut collector = FieldCollector::new();
        event.record(&mut collector);
        let message = collector.into_message();

        let log_event = Arc::new(LogEvent {
            timestamp: Utc::now(),
            level: metadata.level().to_string(),
            target: metadata.target().to_string(),
            message,
            module_path: metadata.module_path().map(|s| s.to_string()),
            file: metadata.file().map(|s| s.to_string()),
            line: metadata.line(),
        });

        // Reentrancy check: if log_service is currently processing a log event
        // on this thread, bypass the queue and write directly to stderr.
        if IN_LOG_PROCESSING.with(|f| f.get()) {
            format_to_stderr(&log_event);
            return;
        }

        // Normal path: non-blocking send to the broadcast queue
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
                        // Activate reentrancy guard: any tracing events emitted
                        // during this block will bypass the LogQueue and write
                        // directly to stderr via DaemonLayer's fallback path.
                        let _guard = LogProcessingGuard::enter();

                        // Console output (always active) -- human-readable format
                        format_to_stderr(&event);

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
                        tracing::warn!(skipped = n, "LogService lagged, some messages were dropped");
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
        let _guard = LogProcessingGuard::enter();
        format_to_stderr(&event);

        #[cfg(feature = "file-logging")]
        if let Some((ref mut writer, _)) = file_writer {
            use std::io::Write;
            let json_line = format_event_json(&event);
            let _ = writeln!(writer, "{}", json_line);
        }
    }

    tracing::info!("LogService shutting down (Priority: SYSTEM)");
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
