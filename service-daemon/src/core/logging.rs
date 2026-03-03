use chrono::{DateTime, Utc};
#[cfg(feature = "file-logging")]
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::cell::Cell;
use std::sync::{Arc, OnceLock};
use tokio::sync::broadcast;
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

/// Log severity level with zero heap allocation.
///
/// Replaces the previous `String` field with a 1-byte enum, eliminating
/// a per-event heap allocation for level formatting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "file-logging", derive(Serialize, Deserialize))]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    /// Converts from tracing's `Level` type. Zero allocation.
    pub fn from_tracing(level: &tracing::Level) -> Self {
        match *level {
            tracing::Level::ERROR => Self::Error,
            tracing::Level::WARN => Self::Warn,
            tracing::Level::INFO => Self::Info,
            tracing::Level::DEBUG => Self::Debug,
            tracing::Level::TRACE => Self::Trace,
        }
    }

    /// Returns the string representation of this log level.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN",
            Self::Info => "INFO",
            Self::Debug => "DEBUG",
            Self::Trace => "TRACE",
        }
    }

    /// Returns the ANSI color escape code pair for console rendering.
    pub fn ansi_color(&self) -> (&'static str, &'static str) {
        match self {
            Self::Error => ("\x1b[31m", "\x1b[0m"), // Red
            Self::Warn => ("\x1b[33m", "\x1b[0m"),  // Yellow
            Self::Info => ("\x1b[32m", "\x1b[0m"),  // Green
            Self::Debug => ("\x1b[36m", "\x1b[0m"), // Cyan
            Self::Trace => ("\x1b[37m", "\x1b[0m"), // White/Gray
        }
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Represents a captured log event with structured metadata.
///
/// This is the canonical log event format used throughout the daemon.
/// When the `file-logging` feature is enabled, events are serialized to JSON
/// and persisted to disk with automatic rotation.
///
/// # ID Fields
///
/// The `service_id`, `message_id`, and `source_id` fields are automatically
/// extracted from the current `tracing::Span` context by `DaemonLayer`. They
/// are `None` for log events that occur outside a service or trigger Span
/// (e.g., during daemon initialization).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "file-logging", derive(Serialize, Deserialize))]
pub struct LogEvent {
    pub timestamp: DateTime<Utc>,
    pub level: LogLevel,
    pub target: Cow<'static, str>,
    pub message: String,
    pub module_path: Option<Cow<'static, str>>,
    pub file: Option<Cow<'static, str>>,
    pub line: Option<u32>,
    /// The `ServiceId` of the service that produced this event, extracted from
    /// the enclosing `tracing::Span` created by `ServiceSupervisor` or
    /// `TracingInterceptor`. `None` when outside a service context.
    #[cfg_attr(
        feature = "file-logging",
        serde(skip_serializing_if = "Option::is_none")
    )]
    pub service_id: Option<String>,
    /// The globally unique message ID for trigger events, extracted from the
    /// `TracingInterceptor` Span. `None` for non-trigger log events.
    #[cfg_attr(
        feature = "file-logging",
        serde(skip_serializing_if = "Option::is_none")
    )]
    pub message_id: Option<String>,
    /// The `ServiceId` of the service that originally published the trigger
    /// event. Extracted from the trigger Span context. `None` for non-trigger
    /// log events.
    #[cfg_attr(
        feature = "file-logging",
        serde(skip_serializing_if = "Option::is_none")
    )]
    pub source_id: Option<String>,
    /// Structured error chain captured via `record_error`. Contains the full
    /// `Debug` representation of the error, including any backtrace information
    /// from `anyhow` or `std::backtrace`.
    #[cfg_attr(
        feature = "file-logging",
        serde(skip_serializing_if = "Option::is_none")
    )]
    pub error_chain: Option<String>,
}

/// A high-performance broadcast queue for log events.
/// This allows multiple consumers (like LogService and potentially a DevConsole).
pub struct LogQueue {
    pub tx: broadcast::Sender<Arc<LogEvent>>,
}

/// Default broadcast channel capacity for the log queue.
///
/// Each slot stores an `Arc<LogEvent>` (8 bytes on 64-bit), so the total
/// baseline memory cost is approximately `capacity * 8` bytes (~512 KB at
/// the default of 65,536).
const DEFAULT_LOG_QUEUE_CAPACITY: usize = 65536;

/// Global log queue capacity override, set via [`set_log_queue_capacity()`].
/// Must be configured before the first call to `get_log_queue()` (which is
/// triggered by `init_logging()` or the first tracing event).
static LOG_QUEUE_CAPACITY: OnceLock<usize> = OnceLock::new();

/// Sets the broadcast channel capacity for the log event queue.
///
/// Must be called **before** `init_logging()` or `ServiceDaemon::run()` to
/// take effect. If not called, defaults to 65,536 slots (~512 KB).
///
/// # When to Use
///
/// - **Resource-constrained environments**: Reduce to `4096` or `8192` to
///   lower memory usage.
/// - **High-throughput services**: Increase beyond `65536` if you observe
///   `LogService lagged` warnings.
///
/// # Example
/// ```rust,ignore
/// use service_daemon::set_log_queue_capacity;
///
/// // Reduce queue for a lightweight embedded daemon
/// set_log_queue_capacity(4096);
/// service_daemon::core::logging::init_logging();
/// ```
pub fn set_log_queue_capacity(capacity: usize) {
    let _ = LOG_QUEUE_CAPACITY.set(capacity);
}

impl Default for LogQueue {
    fn default() -> Self {
        let capacity = LOG_QUEUE_CAPACITY
            .get()
            .copied()
            .unwrap_or(DEFAULT_LOG_QUEUE_CAPACITY);
        let (tx, _) = broadcast::channel(capacity);
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

/// Time-based log rotation strategy.
///
/// Controls how frequently the file appender rotates to a new log file.
/// Only available when the `file-logging` feature is enabled.
#[cfg(feature = "file-logging")]
#[derive(Debug, Clone, Copy, Default)]
pub enum RotationPolicy {
    /// Rotate daily (default). Produces files like `prefix.2026-03-03`.
    #[default]
    Daily,
    /// Rotate hourly. Suitable for high-volume services.
    Hourly,
    /// Never rotate. Single file, relies on external log rotation tools.
    Never,
}

/// Configuration for file-based log persistence.
///
/// Controls the output directory, file prefix, rotation strategy, and
/// retention limit. Only available when the `file-logging` feature is enabled.
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
    /// Time-based rotation strategy. Default: `RotationPolicy::Daily`.
    pub rotation: RotationPolicy,
    /// Maximum number of log files to retain on disk. When a new file
    /// is created and this limit is exceeded, the oldest matching file
    /// is deleted. `None` means no cleanup. Default: `Some(30)`.
    pub max_log_files: Option<usize>,
}

#[cfg(feature = "file-logging")]
impl FileLogConfig {
    /// Creates a new file log configuration with sensible defaults.
    ///
    /// Uses daily rotation and retains the last 30 log files.
    ///
    /// # Arguments
    /// * `directory` - Path to the log output directory (created if missing).
    /// * `file_prefix` - Prefix for rotated log file names.
    #[must_use]
    pub fn new(directory: impl Into<String>, file_prefix: impl Into<String>) -> Self {
        Self {
            directory: directory.into(),
            file_prefix: file_prefix.into(),
            rotation: RotationPolicy::Daily,
            max_log_files: Some(30),
        }
    }
}

#[cfg(feature = "file-logging")]
impl Default for FileLogConfig {
    fn default() -> Self {
        Self {
            directory: "logs".to_string(),
            file_prefix: "daemon".to_string(),
            rotation: RotationPolicy::Daily,
            max_log_files: Some(30),
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

/// One-line initialization: registers `DaemonLayer` + `EnvFilter` as the
/// global tracing subscriber.
///
/// Reads the `RUST_LOG` environment variable for log-level configuration.
/// Falls back to `"info"` if `RUST_LOG` is not set.
///
/// For custom subscriber stacks (e.g., adding Sentry or OpenTelemetry layers),
/// compose your own subscriber using `DaemonLayer` directly:
///
/// ```rust,ignore
/// use service_daemon::core::logging::DaemonLayer;
/// use tracing_subscriber::prelude::*;
///
/// tracing_subscriber::registry()
///     .with(tracing_subscriber::EnvFilter::new("info"))
///     .with(DaemonLayer)
///     .with(my_sentry_layer)
///     .init();
/// ```
///
/// File logging is configured separately via [`enable_file_logging()`] and
/// consumed by the independent `file_log_service`.
///
/// # Panics
/// Panics if a global subscriber has already been set. Use
/// [`try_init_logging()`] in test environments where multiple tests may race.
pub fn init_logging() {
    use tracing_subscriber::prelude::*;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(DaemonLayer)
        .init();
}

/// Fallible variant of [`init_logging()`] for test environments.
///
/// Identical to `init_logging()` but returns `Err` instead of panicking
/// when a global subscriber has already been set. This is safe to call
/// from multiple `#[tokio::test]` functions running in parallel.
///
/// # Example
/// ```rust,ignore
/// #[tokio::test]
/// async fn my_test() {
///     let _ = service_daemon::core::logging::try_init_logging();
///     // ... test logic
/// }
/// ```
pub fn try_init_logging() -> Result<(), tracing_subscriber::util::TryInitError> {
    use tracing_subscriber::prelude::*;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(DaemonLayer)
        .try_init()
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
    /// Captured error chain from `record_error` or an `error` named field.
    error_chain: Option<String>,
}

impl FieldCollector {
    fn new() -> Self {
        Self {
            message: String::new(),
            fields: Vec::new(),
            error_chain: None,
        }
    }

    /// Builds the final message string.
    /// If structured fields are present, appends them as `{ key=value, ... }`.
    fn build_message(&self) -> String {
        if self.fields.is_empty() {
            self.message.clone()
        } else {
            let pairs: Vec<String> = self
                .fields
                .iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect();
            format!("{} {{ {} }}", self.message, pairs.join(", "))
        }
    }

    /// Extracts the captured error chain, if any.
    fn take_error(&mut self) -> Option<String> {
        self.error_chain.take()
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
        } else if field.name() == "error" {
            // Capture error fields separately for structured logging
            self.error_chain = Some(formatted);
        } else {
            self.fields.push((field.name().to_string(), formatted));
        }
    }
}

/// Renders a log event into the provided buffer with ANSI color codes.
///
/// The buffer is cleared but NOT deallocated, allowing memory reuse across
/// successive calls within a batch loop.
fn render_to_buf(event: &LogEvent, buf: &mut String) {
    buf.clear();
    let (color, reset) = event.level.ansi_color();

    use std::fmt::Write;
    let _ = write!(
        buf,
        "{} {}{:<5}{} [{}] {}",
        event.timestamp.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
        color,
        event.level.as_str(),
        reset,
        event.target,
        event.message,
    );

    // Append IDs when present (inside a service/trigger Span)
    if let Some(ref sid) = event.service_id {
        let _ = write!(buf, " service_id={}", sid);
    }
    if let Some(ref mid) = event.message_id {
        let _ = write!(buf, " message_id={}", mid);
    }
    if let Some(ref src) = event.source_id {
        let _ = write!(buf, " source_id={}", src);
    }
    if let Some(ref err) = event.error_chain {
        let _ = write!(buf, " error={}", err);
    }
}

/// Renders a log event to an allocated String for testing.
///
/// Convenience wrapper around `render_to_buf` that allocates a fresh buffer.
/// For batch processing, prefer `render_to_buf` with a reusable buffer.
#[cfg(test)]
fn render_to_string(event: &LogEvent) -> String {
    let mut buf = String::with_capacity(256);
    render_to_buf(event, &mut buf);
    buf
}

/// Renders a log event to stderr using ANSI color coding and structured fields.
///
/// Thin wrapper around `render_to_buf` that performs a single atomic write
/// to stderr to avoid interleaved output from concurrent threads.
fn render_to_stderr(event: &LogEvent) {
    use std::io::{self, Write};

    let mut buf = String::with_capacity(256);
    render_to_buf(event, &mut buf);
    buf.push('\n');

    let stderr = io::stderr();
    let _ = stderr.lock().write_all(buf.as_bytes());
}

/// A non-blocking tracing Layer that captures events and pushes them to the LogQueue.
///
/// When reentrancy is detected (i.e., `log_service` emits a tracing event while
/// processing a log), the event bypasses the queue and is written directly to stderr
/// to prevent infinite recursion.
///
/// # Span Context Extraction
///
/// `DaemonLayer` requires `LookupSpan` on the subscriber so it can walk the
/// current Span chain and extract `service_id`, `message_id`, and `source_id`
/// fields that were injected by `ServiceSupervisor::on_running` and
/// `TracingInterceptor`. Events outside any Span will have `None` for these IDs.
pub struct DaemonLayer;

impl<S> Layer<S> for DaemonLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    /// Captures Span field values (`service_id`, `message_id`, etc.) into the
    /// Span's extensions on creation. These are later read by `extract_span_ids`
    /// during event processing.
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        if let Some(span) = ctx.span(id) {
            let mut visitor = SpanFieldVisitor::default();
            attrs.record(&mut visitor);

            // Only store if at least one known field was found
            if visitor.fields.service_id.is_some()
                || visitor.fields.message_id.is_some()
                || visitor.fields.source_id.is_some()
            {
                span.extensions_mut().insert(visitor.fields);
            }
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let metadata = event.metadata();

        // Collect message and structured fields from the event
        let mut collector = FieldCollector::new();
        event.record(&mut collector);
        let message = collector.build_message();
        let error_chain = collector.take_error();

        // Walk the Span chain to extract service/trigger IDs
        let (service_id, message_id, source_id) = extract_span_ids(&ctx, event);

        let log_event = Arc::new(LogEvent {
            timestamp: Utc::now(),
            level: LogLevel::from_tracing(metadata.level()),
            target: Cow::Borrowed(metadata.target()),
            message,
            module_path: metadata.module_path().map(Cow::Borrowed),
            file: metadata.file().map(Cow::Borrowed),
            line: metadata.line(),
            service_id,
            message_id,
            source_id,
            error_chain,
        });

        // Reentrancy check: if log_service is currently processing a log event
        // on this thread, bypass the queue and write directly to stderr.
        if IN_LOG_PROCESSING.with(|f| f.get()) {
            render_to_stderr(&log_event);
            return;
        }

        // Normal path: non-blocking send to the broadcast queue
        let _ = get_log_queue().tx.send(log_event);
    }
}

/// Walks the current Span scope to extract `service_id`, `message_id`, and
/// `source_id` fields from ancestor Spans.
///
/// These fields are injected by:
/// - `ServiceSupervisor::on_running` — creates `info_span!("service", service_id = ...)`
/// - `TracingInterceptor` — creates `info_span!("trigger", service_id = ..., message_id = ...)`
///
/// The walk proceeds from innermost (current) to outermost Span, returning
/// the first value found for each field.
fn extract_span_ids<S>(
    ctx: &Context<'_, S>,
    event: &Event<'_>,
) -> (Option<String>, Option<String>, Option<String>)
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    let mut service_id = None;
    let mut message_id = None;
    let mut source_id = None;

    // `event_scope()` returns an iterator over Span references from innermost
    // to outermost. We scan for known field names stored in Span extensions.
    if let Some(scope) = ctx.event_scope(event) {
        for span in scope {
            let extensions = span.extensions();
            if let Some(fields) = extensions.get::<SpanFields>() {
                if service_id.is_none() {
                    service_id.clone_from(&fields.service_id);
                }
                if message_id.is_none() {
                    message_id.clone_from(&fields.message_id);
                }
                if source_id.is_none() {
                    source_id.clone_from(&fields.source_id);
                }
            }
            // Early exit if all IDs are found
            if service_id.is_some() && message_id.is_some() && source_id.is_some() {
                break;
            }
        }
    }

    (service_id, message_id, source_id)
}

/// Storage for extracted span field values, attached to each Span via extensions.
///
/// When `DaemonLayer` sees a new Span with known field names (`service_id`,
/// `message_id`, etc.), it stores their values in a `SpanFields` instance
/// within the Span's extensions. These values are later read by
/// `extract_span_ids` during event processing.
#[derive(Debug, Default)]
struct SpanFields {
    service_id: Option<String>,
    message_id: Option<String>,
    source_id: Option<String>,
}

/// Visitor that extracts known ID fields from Span attributes during creation.
///
/// Recognizes:
/// - `service_id` — from `ServiceSupervisor::on_running` and `TracingInterceptor`
/// - `message_id` — from `TracingInterceptor` (trigger dispatch)
/// - `instance_id` — from `TracingInterceptor`, mapped to `source_id`
///
/// All other fields are ignored. Values are captured via `Display` formatting.
#[derive(Debug, Default)]
struct SpanFieldVisitor {
    fields: SpanFields,
}

impl tracing::field::Visit for SpanFieldVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let formatted = format!("{:?}", value);
        match field.name() {
            "service_id" => self.fields.service_id = Some(formatted),
            "message_id" => self.fields.message_id = Some(formatted),
            "instance_id" => self.fields.source_id = Some(formatted),
            _ => {} // Ignore unknown fields
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "service_id" => self.fields.service_id = Some(value.to_string()),
            "message_id" => self.fields.message_id = Some(value.to_string()),
            "instance_id" => self.fields.source_id = Some(value.to_string()),
            _ => {}
        }
    }
}

/// Formats a `LogEvent` as a structured JSON string for file persistence.
///
/// Output includes `level`, `time` (ISO 8601), `target`,
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

/// Maximum number of events to accumulate in a single drain cycle before
/// flushing. This is a safety cap — under normal load `try_recv()` returns
/// `Err(Empty)` long before this limit, so the batch is flushed immediately.
/// The cap only activates during extreme bursts to bound memory usage and
/// prevent a single flush pass from monopolizing the tokio worker thread.
const BATCH_SIZE: usize = 1024;

/// A background service that consumes the LogQueue and renders events to stderr.
/// It uses ShutdownOrder::SYSTEM (100) to ensure it exits last.
///
/// ## Responsibility
/// Console output **only**. File persistence is handled by the independent
/// `file_log_service` (behind the `file-logging` feature gate).
///
/// ## Fill-the-Valley Strategy
/// Instead of processing events one-by-one with per-event lock acquisition,
/// this service uses a batch buffer:
/// 1. Block until at least one event arrives (`recv().await`).
/// 2. Greedily drain all immediately available events via `try_recv()`.
/// 3. Flush the entire batch in one pass with a single reentrancy guard.
#[service_daemon::service(priority = service_daemon::ServicePriority::SYSTEM, tags = ["__log__"])]
pub async fn log_service() -> anyhow::Result<()> {
    let mut rx = get_log_queue().tx.subscribe();
    let mut buffer: Vec<Arc<LogEvent>> = Vec::with_capacity(BATCH_SIZE);

    while !service_daemon::is_shutdown() {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(event) => {
                        buffer.push(event);

                        // Greedily drain all immediately available events
                        while buffer.len() < BATCH_SIZE {
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
                                    use std::io::Write;
                                    let stderr = std::io::stderr();
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
            _ = service_daemon::wait_shutdown() => {
                break;
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
                use std::io::Write;
                let stderr = std::io::stderr();
                let _ = stderr.lock().write_all(render_buf.as_bytes());
            }
        }
    }

    tracing::info!("LogService shutting down (Priority: SYSTEM)");
    Ok(())
}

/// An independent background service for file-based JSON log persistence.
///
/// Subscribes to the same `LogQueue` broadcast channel as `log_service`,
/// consuming events independently. Each consumer has its own cursor into
/// the broadcast ring buffer — neither blocks the other.
///
/// ## Activation
/// Only runs when `enable_file_logging()` has been called before daemon start
/// AND the `file-logging` Cargo feature is enabled at compile time.
/// When `FILE_LOG_CONFIG` is not set, this service exits immediately.
///
/// ## Output Format
/// JSON lines (one JSON object per line), written to daily-rotating files
/// via `tracing-appender::rolling::daily`. File names follow the pattern:
/// `{prefix}.YYYY-MM-DD`.
#[cfg(feature = "file-logging")]
#[service_daemon::service(priority = service_daemon::ServicePriority::SYSTEM, tags = ["__file_log__"])]
pub async fn file_log_service() -> anyhow::Result<()> {
    // Exit immediately if file logging was not configured
    let config = match FILE_LOG_CONFIG.get() {
        Some(config) => config,
        None => return Ok(()),
    };

    let rotation = match config.rotation {
        RotationPolicy::Daily => tracing_appender::rolling::Rotation::DAILY,
        RotationPolicy::Hourly => tracing_appender::rolling::Rotation::HOURLY,
        RotationPolicy::Never => tracing_appender::rolling::Rotation::NEVER,
    };

    let mut builder = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(rotation)
        .filename_prefix(&config.file_prefix);

    if let Some(max_files) = config.max_log_files {
        builder = builder.max_log_files(max_files);
    }

    let file_appender = builder
        .build(&config.directory)
        .expect("Failed to initialize rolling file appender");
    let (mut writer, _guard) = tracing_appender::non_blocking(file_appender);

    let mut rx = get_log_queue().tx.subscribe();
    let mut buffer: Vec<Arc<LogEvent>> = Vec::with_capacity(BATCH_SIZE);

    while !service_daemon::is_shutdown() {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(event) => {
                        buffer.push(event);

                        while buffer.len() < BATCH_SIZE {
                            match rx.try_recv() {
                                Ok(event) => buffer.push(event),
                                Err(_) => break,
                            }
                        }

                        // Flush batch to file
                        {
                            use std::io::Write;
                            for event in buffer.drain(..) {
                                let json_line = format_event_json(&event);
                                let _ = writeln!(writer, "{}", json_line);
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            skipped = n,
                            "FileLogService lagged, some messages were not persisted to file"
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = service_daemon::wait_shutdown() => {
                break;
            }
        }
    }

    // Drain remaining events to file before exiting
    while let Ok(event) = rx.try_recv() {
        buffer.push(event);
    }
    if !buffer.is_empty() {
        use std::io::Write;
        for event in buffer.drain(..) {
            let json_line = format_event_json(&event);
            let _ = writeln!(writer, "{}", json_line);
        }
    }

    tracing::info!("FileLogService shutting down (Priority: SYSTEM)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Helper: build a synthetic LogEvent with specified fields for testing
    // -----------------------------------------------------------------------
    fn make_event(
        level: LogLevel,
        message: &str,
        service_id: Option<&str>,
        message_id: Option<&str>,
        source_id: Option<&str>,
        error_chain: Option<&str>,
    ) -> LogEvent {
        LogEvent {
            timestamp: chrono::Utc::now(),
            level,
            target: Cow::Borrowed("test::target"),
            message: message.to_string(),
            module_path: None,
            file: None,
            line: None,
            service_id: service_id.map(|s| s.to_string()),
            message_id: message_id.map(|s| s.to_string()),
            source_id: source_id.map(|s| s.to_string()),
            error_chain: error_chain.map(|s| s.to_string()),
        }
    }

    // =======================================================================
    // 5A: ConsoleRenderer ANSI color output tests
    // =======================================================================

    #[test]
    fn render_info_level_contains_green_ansi_code() {
        let event = make_event(LogLevel::Info, "hello world", None, None, None, None);
        let output = render_to_string(&event);
        // Green foreground: \x1b[32m
        assert!(
            output.contains("\x1b[32m"),
            "INFO output should contain green ANSI code, got: {}",
            output
        );
        // Reset code: \x1b[0m
        assert!(
            output.contains("\x1b[0m"),
            "output should contain ANSI reset code"
        );
    }

    #[test]
    fn render_error_level_contains_red_ansi_code() {
        let event = make_event(LogLevel::Error, "something broke", None, None, None, None);
        let output = render_to_string(&event);
        // Red foreground: \x1b[31m
        assert!(
            output.contains("\x1b[31m"),
            "ERROR output should contain red ANSI code, got: {}",
            output
        );
    }

    #[test]
    fn render_warn_level_contains_yellow_ansi_code() {
        let event = make_event(LogLevel::Warn, "caution", None, None, None, None);
        let output = render_to_string(&event);
        // Yellow foreground: \x1b[33m
        assert!(
            output.contains("\x1b[33m"),
            "WARN output should contain yellow ANSI code, got: {}",
            output
        );
    }

    #[test]
    fn render_debug_level_contains_cyan_ansi_code() {
        let event = make_event(LogLevel::Debug, "verbose detail", None, None, None, None);
        let output = render_to_string(&event);
        assert!(
            output.contains("\x1b[36m"),
            "DEBUG output should contain cyan ANSI code, got: {}",
            output
        );
    }

    // =======================================================================
    // 5A continued: ID and error_chain rendering in console output
    // =======================================================================

    #[test]
    fn render_includes_service_id_when_present() {
        let event = make_event(LogLevel::Info, "msg", Some("svc-abc-123"), None, None, None);
        let output = render_to_string(&event);
        assert!(
            output.contains("service_id=svc-abc-123"),
            "output should contain service_id, got: {}",
            output
        );
    }

    #[test]
    fn render_includes_all_ids_when_present() {
        let event = make_event(
            LogLevel::Info,
            "triggered",
            Some("svc-001"),
            Some("msg-002"),
            Some("src-003"),
            None,
        );
        let output = render_to_string(&event);
        assert!(output.contains("service_id=svc-001"), "missing service_id");
        assert!(output.contains("message_id=msg-002"), "missing message_id");
        assert!(output.contains("source_id=src-003"), "missing source_id");
    }

    #[test]
    fn render_includes_error_chain_when_present() {
        let event = make_event(
            LogLevel::Error,
            "operation failed",
            None,
            None,
            None,
            Some("connection refused"),
        );
        let output = render_to_string(&event);
        assert!(
            output.contains("error=connection refused"),
            "output should contain error chain, got: {}",
            output
        );
    }

    #[test]
    fn render_omits_ids_when_none() {
        let event = make_event(LogLevel::Info, "init phase", None, None, None, None);
        let output = render_to_string(&event);
        assert!(
            !output.contains("service_id="),
            "should not contain service_id when None"
        );
        assert!(
            !output.contains("message_id="),
            "should not contain message_id when None"
        );
        assert!(
            !output.contains("source_id="),
            "should not contain source_id when None"
        );
        assert!(
            !output.contains("error="),
            "should not contain error when None"
        );
    }

    // =======================================================================
    // 5B: LogEvent ID propagation via DaemonLayer + Span context
    // =======================================================================

    /// Installs a temporary subscriber with DaemonLayer, runs the closure,
    /// and returns collected LogEvents from the broadcast queue.
    ///
    /// Uses `tracing::subscriber::with_default` for test isolation — does NOT
    /// set a global subscriber, so tests can run in parallel.
    fn collect_events_with_daemon_layer(f: impl FnOnce()) -> Vec<Arc<LogEvent>> {
        use tracing_subscriber::prelude::*;

        let mut rx = get_log_queue().tx.subscribe();

        // Drain any stale events from prior tests sharing the global queue
        while rx.try_recv().is_ok() {}

        let subscriber = tracing_subscriber::registry().with(DaemonLayer);

        tracing::subscriber::with_default(subscriber, f);

        // Collect only events produced during our closure
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    }

    #[test]
    fn daemon_layer_captures_service_id_from_span() {
        let events = collect_events_with_daemon_layer(|| {
            let span = tracing::info_span!("service", service_id = "test-svc-42",);
            let _enter = span.enter();
            tracing::info!("svc_id_test_marker");
        });

        let event = events
            .iter()
            .find(|e| e.message.contains("svc_id_test_marker"))
            .expect("should have captured the event");

        assert_eq!(
            event.service_id.as_deref(),
            Some("test-svc-42"),
            "service_id should be extracted from Span"
        );
    }

    #[test]
    fn daemon_layer_captures_message_id_from_nested_span() {
        let events = collect_events_with_daemon_layer(|| {
            let service_span = tracing::info_span!("service", service_id = "parent-svc",);
            let _svc_enter = service_span.enter();

            let trigger_span = tracing::info_span!(
                "trigger",
                service_id = "trigger-svc",
                message_id = "msg-xyz-789",
                instance_id = "source-abc",
            );
            let _trig_enter = trigger_span.enter();
            tracing::info!("nested_span_test_marker");
        });

        let event = events
            .iter()
            .find(|e| e.message.contains("nested_span_test_marker"))
            .expect("should have captured the event");

        assert_eq!(
            event.service_id.as_deref(),
            Some("trigger-svc"),
            "service_id should come from innermost span"
        );
        assert_eq!(
            event.message_id.as_deref(),
            Some("msg-xyz-789"),
            "message_id should be extracted from trigger span"
        );
        assert_eq!(
            event.source_id.as_deref(),
            Some("source-abc"),
            "source_id should be mapped from instance_id"
        );
    }

    #[test]
    fn daemon_layer_returns_none_ids_outside_span() {
        let events = collect_events_with_daemon_layer(|| {
            tracing::info!("no_span_ctx_marker");
        });

        let event = events
            .iter()
            .find(|e| e.message.contains("no_span_ctx_marker"))
            .expect("should capture the event");

        assert!(
            event.service_id.is_none(),
            "service_id should be None outside span"
        );
        assert!(
            event.message_id.is_none(),
            "message_id should be None outside span"
        );
        assert!(
            event.source_id.is_none(),
            "source_id should be None outside span"
        );
    }

    // =======================================================================
    // 5C: Async queue delivery (non-blocking) verification
    // =======================================================================

    #[test]
    fn daemon_layer_delivers_events_via_broadcast_queue() {
        let events = collect_events_with_daemon_layer(|| {
            tracing::info!("queue_alpha_marker");
            tracing::warn!("queue_beta_marker");
        });

        let alpha = events
            .iter()
            .find(|e| e.message.contains("queue_alpha_marker"))
            .expect("alpha event should be in the queue");
        let beta = events
            .iter()
            .find(|e| e.message.contains("queue_beta_marker"))
            .expect("beta event should be in the queue");

        assert_eq!(alpha.level, LogLevel::Info);
        assert_eq!(beta.level, LogLevel::Warn);
    }

    #[test]
    fn daemon_layer_send_is_non_blocking() {
        // Verify that DaemonLayer::on_event returns immediately even when
        // no receiver is actively consuming. The broadcast channel with
        // capacity 1024 absorbs events without blocking the caller.
        let event_count = 100;
        let events = collect_events_with_daemon_layer(|| {
            for i in 0..event_count {
                tracing::debug!(index = i, "burst_marker");
            }
        });

        let burst_events: Vec<_> = events
            .iter()
            .filter(|e| e.message.contains("burst_marker"))
            .collect();

        assert_eq!(
            burst_events.len(),
            event_count,
            "all {} burst events should arrive in the queue without blocking",
            event_count
        );
    }
}
