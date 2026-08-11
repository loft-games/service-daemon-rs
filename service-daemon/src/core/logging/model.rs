use chrono::{DateTime, Utc};
#[cfg(feature = "file-logging")]
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::Level;
use uuid::Uuid;

use std::borrow::Cow;
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::{Arc, OnceLock};

use crate::models::{ServiceInstanceId, service::TriggerInstanceId};

/// Log severity level stored as an enum.
///
/// Formatting returns static level names.
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
    /// Converts from tracing's `Level` type without heap allocation.
    pub fn from_tracing(level: &Level) -> Self {
        match *level {
            Level::ERROR => Self::Error,
            Level::WARN => Self::Warn,
            Level::INFO => Self::Info,
            Level::DEBUG => Self::Debug,
            Level::TRACE => Self::Trace,
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

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Represents a captured log event with structured metadata.
///
/// This is the log event format used throughout the daemon.
/// When the `file-logging` feature is enabled, events are serialized to JSON
/// and persisted to disk with automatic rotation.
///
/// # ID Fields
///
/// The `service_instance_id`, `message_id`, and `trigger_instance_id` fields are automatically
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
    #[cfg_attr(not(feature = "file-logging"), allow(dead_code))]
    pub module_path: Option<Cow<'static, str>>,
    #[cfg_attr(not(feature = "file-logging"), allow(dead_code))]
    pub file: Option<Cow<'static, str>>,
    #[cfg_attr(not(feature = "file-logging"), allow(dead_code))]
    pub line: Option<u32>,
    /// The `ServiceInstanceId` of the service that produced this event.
    #[cfg_attr(
        feature = "file-logging",
        serde(skip_serializing_if = "Option::is_none")
    )]
    pub service_instance_id: Option<ServiceInstanceId>,
    /// The `ServiceInstanceId` of the service that originally emitted the event.
    /// Used for causal topology correlation.
    #[cfg_attr(
        feature = "file-logging",
        serde(skip_serializing_if = "Option::is_none")
    )]
    pub source_service_instance_id: Option<ServiceInstanceId>,
    /// Message ID for causal tracing.
    #[cfg_attr(
        feature = "file-logging",
        serde(skip_serializing_if = "Option::is_none")
    )]
    pub message_id: Option<Uuid>,
    /// The trigger instance identifier, combining `ServiceInstanceId` and sequence
    /// number. Extracted from span fields or native TriggerInstanceId extension.
    #[cfg_attr(
        feature = "file-logging",
        serde(skip_serializing_if = "Option::is_none")
    )]
    pub trigger_instance_id: Option<TriggerInstanceId>,
    /// Structured error chain captured via `record_error`.
    #[cfg_attr(
        feature = "file-logging",
        serde(skip_serializing_if = "Option::is_none")
    )]
    pub error_chain: Option<String>,
}

/// Broadcast queue for log events.
/// Multiple consumers, such as stderr and file logging, subscribe independently.
pub struct LogQueue {
    pub tx: broadcast::Sender<Arc<LogEvent>>,
}

/// Default number of events each consumer drains per batch cycle.
///
/// This is the sole user-facing knob for log throughput tuning.
/// Queue capacity is derived automatically as
/// `batch_size * LOG_QUEUE_BATCH_MULTIPLIER`.
pub(super) const DEFAULT_BATCH_SIZE: usize = 128;

/// Maximum accepted log batch size.
///
/// The derived broadcast queue capacity is this value multiplied by 4.
pub const MAX_LOG_BATCH_SIZE: usize = 1 << 20;

/// Ratio of broadcast queue capacity to batch size.
///
/// A multiplier of 4 means the queue can buffer 4 full drain cycles
/// of burst before lagging occurs, providing adequate headroom for
/// temporary producer-consumer imbalance.
pub(super) const LOG_QUEUE_BATCH_MULTIPLIER: usize = 4;

pub(super) const MAX_LOG_QUEUE_CAPACITY: usize = MAX_LOG_BATCH_SIZE * LOG_QUEUE_BATCH_MULTIPLIER;

/// Error returned when configuring the log batch size fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogBatchSizeError {
    /// The requested batch size exceeds [`MAX_LOG_BATCH_SIZE`].
    TooLarge { requested: usize, max: usize },
    /// The log queue has already been configured or initialized.
    AlreadyInitialized { requested: usize, active: usize },
}

impl fmt::Display for LogBatchSizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { requested, max } => {
                write!(
                    f,
                    "log batch size {requested} exceeds maximum supported size {max}"
                )
            }
            Self::AlreadyInitialized { requested, active } => {
                write!(
                    f,
                    "log batch size already initialized to {active}; requested {requested}"
                )
            }
        }
    }
}

impl std::error::Error for LogBatchSizeError {}

#[cfg(all(test, feature = "file-logging"))]
mod tests {
    use super::*;

    #[test]
    fn file_logging_serializes_service_instance_ids_as_uuid_wrappers() {
        let service_instance_id = ServiceInstanceId::new(
            Uuid::parse_str("019fe746-6158-7403-82c9-ac72cad515ec").unwrap(),
        );
        let source_service_instance_id = ServiceInstanceId::new(
            Uuid::parse_str("019fe746-6158-7403-82c9-ac6ccaa91ef5").unwrap(),
        );
        let trigger_instance_id = TriggerInstanceId::new(service_instance_id, 42);
        let event = LogEvent {
            timestamp: Utc::now(),
            level: LogLevel::Info,
            target: Cow::Borrowed("test"),
            message: "serialized identity".to_string(),
            module_path: None,
            file: None,
            line: None,
            service_instance_id: Some(service_instance_id),
            source_service_instance_id: Some(source_service_instance_id),
            message_id: Some(Uuid::parse_str("019fe746-6158-7403-82c9-ac8b7e502cb3").unwrap()),
            trigger_instance_id: Some(trigger_instance_id),
            error_chain: None,
        };

        let json = serde_json::to_value(&event).expect("log event should serialize");
        assert_eq!(
            json["service_instance_id"],
            serde_json::json!("019fe746-6158-7403-82c9-ac72cad515ec")
        );
        assert_eq!(
            json["source_service_instance_id"],
            serde_json::json!("019fe746-6158-7403-82c9-ac6ccaa91ef5")
        );
        assert_eq!(
            json["trigger_instance_id"]["service_instance_id"],
            serde_json::json!("019fe746-6158-7403-82c9-ac72cad515ec")
        );
        assert_eq!(json["trigger_instance_id"]["seq"], serde_json::json!(42));
        assert!(json.get("service_instance_id_num").is_none());
        assert!(json.get("trigger_instance_service_instance_id").is_none());
    }
}

/// Global batch size override, set via [`set_log_batch_size()`].
/// Must be configured before the first call to `get_log_queue()` (which is
/// triggered by `init_logging()` or the first tracing event).
static LOG_BATCH_SIZE: OnceLock<NonZeroUsize> = OnceLock::new();

pub(super) fn validate_log_batch_size(batch_size: usize) -> Result<(), LogBatchSizeError> {
    if batch_size > MAX_LOG_BATCH_SIZE {
        return Err(LogBatchSizeError::TooLarge {
            requested: batch_size,
            max: MAX_LOG_BATCH_SIZE,
        });
    }
    Ok(())
}

pub(super) fn log_queue_capacity_for_batch_size(
    batch_size: usize,
) -> Result<usize, LogBatchSizeError> {
    validate_log_batch_size(batch_size)?;
    batch_size
        .checked_mul(LOG_QUEUE_BATCH_MULTIPLIER)
        .filter(|capacity| *capacity <= MAX_LOG_QUEUE_CAPACITY)
        .ok_or(LogBatchSizeError::TooLarge {
            requested: batch_size,
            max: MAX_LOG_BATCH_SIZE,
        })
}

/// Returns the effective batch size (user-configured or default).
pub(super) fn effective_batch_size() -> usize {
    LOG_BATCH_SIZE
        .get()
        .map_or(DEFAULT_BATCH_SIZE, |size| size.get())
}

pub(super) fn set_log_batch_size_in(
    storage: &OnceLock<NonZeroUsize>,
    queue_initialized: bool,
    size: NonZeroUsize,
) -> Result<(), LogBatchSizeError> {
    validate_log_batch_size(size.get())?;

    if queue_initialized {
        let requested = size.get();
        let active = storage.get().map_or(DEFAULT_BATCH_SIZE, |size| size.get());
        tracing::warn!(
            requested,
            active,
            "set_log_batch_size: log queue already initialized; call ignored"
        );
        return Err(LogBatchSizeError::AlreadyInitialized { requested, active });
    }

    match storage.set(size) {
        Ok(()) => Ok(()),
        Err(size) => {
            let requested = size.get();
            let active = storage.get().map_or(DEFAULT_BATCH_SIZE, |size| size.get());
            tracing::warn!(
                requested,
                active,
                "set_log_batch_size: batch size already initialized; call ignored"
            );
            Err(LogBatchSizeError::AlreadyInitialized { requested, active })
        }
    }
}

/// Sets the batch processing size for the log service drain cycle.
///
/// Must be called **before** `init_logging()` or the daemon handle's `run()` to
/// take effect. The broadcast queue capacity is automatically derived as
/// `batch_size * 4`.
///
/// # Errors
///
/// Returns [`LogBatchSizeError::TooLarge`] when the requested size exceeds
/// [`MAX_LOG_BATCH_SIZE`]. Returns [`LogBatchSizeError::AlreadyInitialized`]
/// when logging has already observed or accepted a batch size.
///
/// # When to Use
///
/// - **Resource-constrained environments**: Reduce to `256` or `512` to
///   lower memory usage (queue capacity becomes 1,024 or 2,048).
/// - **High-throughput services**: Increase to `2048` or `4096` if you
///   observe `LogService lagged` warnings (queue becomes 8,192 or 16,384).
///
/// # Example
/// ```rust,ignore
/// use std::num::NonZeroUsize;
/// use service_daemon::set_log_batch_size;
///
/// // Reduce batch size for a lightweight embedded daemon
/// // Queue capacity will be 512 * 4 = 2,048 slots
/// if let Some(batch_size) = NonZeroUsize::new(512) {
///     set_log_batch_size(batch_size)?;
/// }
/// service_daemon::init_logging();
/// # Ok::<(), service_daemon::LogBatchSizeError>(())
/// ```
pub fn set_log_batch_size(size: NonZeroUsize) -> Result<(), LogBatchSizeError> {
    set_log_batch_size_in(&LOG_BATCH_SIZE, LOG_QUEUE.get().is_some(), size)
}

impl Default for LogQueue {
    fn default() -> Self {
        // Capacity is derived once here and cached by the outer OnceLock<LogQueue>.
        let batch_size = effective_batch_size();
        let capacity = match log_queue_capacity_for_batch_size(batch_size) {
            Ok(capacity) => capacity,
            Err(error) => {
                panic!("validated log batch size produced invalid queue capacity: {error}")
            }
        };
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }
}

/// Global log queue, initialized on first access.
///
/// Uses `std::sync::OnceLock` for race-free, synchronous initialization.
static LOG_QUEUE: OnceLock<LogQueue> = OnceLock::new();

/// Gets the log queue, initializing it on first call.
pub(crate) fn get_log_queue() -> &'static LogQueue {
    LOG_QUEUE.get_or_init(LogQueue::default)
}
