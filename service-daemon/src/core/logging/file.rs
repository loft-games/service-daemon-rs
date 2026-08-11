use std::io::Write as _;
use std::sync::Arc;

use tokio::sync::broadcast;
use tracing_appender::non_blocking;
use tracing_appender::rolling::{RollingFileAppender, Rotation};

use crate::ServicePriority;
use crate::service;

use super::model::{LogEvent, effective_batch_size, get_log_queue};
use super::render::format_event_json;

/// Time-based log rotation strategy.
///
/// Controls how frequently the file appender rotates to a new log file.
/// Only available when the `file-logging` feature is enabled.
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
/// use service_daemon::FileLogConfig;
///
/// let config = FileLogConfig::new("logs", "app");
/// ```
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

impl FileLogConfig {
    /// Creates a new file log configuration.
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
static FILE_LOG_CONFIG: tokio::sync::OnceCell<FileLogConfig> = tokio::sync::OnceCell::const_new();

/// Enables file-based log persistence with the given configuration.
///
/// Must be called **before** the daemon handle's `run()` to take effect.
/// If not called, the `log_service` will only output to stderr (console).
///
/// # Arguments
/// * `config` - File logging configuration specifying directory and prefix.
///
/// # Example
/// ```rust,ignore
/// use service_daemon::{FileLogConfig, enable_file_logging};
///
/// enable_file_logging(FileLogConfig::new("logs", "my-app"));
/// ```
pub fn enable_file_logging(config: FileLogConfig) {
    let _ = FILE_LOG_CONFIG.set(config);
}

/// An independent background service for file-based JSON log persistence.
///
/// Subscribes to the same `LogQueue` broadcast channel as `log_service`,
/// consuming events independently. Each consumer has its own cursor into
/// the broadcast ring buffer - neither blocks the other.
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
#[service(priority = ServicePriority::SYSTEM, tags = ["__file_log__"])]
pub async fn file_log_service() -> anyhow::Result<()> {
    // If file logging is not configured, stay idle until shutdown instead of
    // exiting immediately. Otherwise supervision treats the clean exit as a
    // successful generation and hot-restarts this SYSTEM service forever.
    let config = match FILE_LOG_CONFIG.get() {
        Some(config) => config,
        None => {
            service_daemon::wait_shutdown().await;
            return Ok(());
        }
    };

    let rotation = match config.rotation {
        RotationPolicy::Daily => Rotation::DAILY,
        RotationPolicy::Hourly => Rotation::HOURLY,
        RotationPolicy::Never => Rotation::NEVER,
    };

    let mut builder = RollingFileAppender::builder()
        .rotation(rotation)
        .filename_prefix(&config.file_prefix);

    if let Some(max_files) = config.max_log_files {
        builder = builder.max_log_files(max_files);
    }

    let file_appender = match builder.build(&config.directory) {
        Ok(file_appender) => file_appender,
        Err(err) => {
            tracing::warn!(
                directory = %config.directory,
                file_prefix = %config.file_prefix,
                error = %err,
                "file logging disabled; continuing with console logging only"
            );
            service_daemon::wait_shutdown().await;
            return Ok(());
        }
    };
    let (mut writer, _guard) = non_blocking(file_appender);

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

                        while buffer.len() < batch_size {
                            match rx.try_recv() {
                                Ok(event) => buffer.push(event),
                                Err(_) => break,
                            }
                        }

                        // Flush batch to file
                        {
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
        }
    }

    // Drain remaining events to file before exiting
    while let Ok(event) = rx.try_recv() {
        buffer.push(event);
    }
    if !buffer.is_empty() {
        for event in buffer.drain(..) {
            let json_line = format_event_json(&event);
            let _ = writeln!(writer, "{}", json_line);
        }
    }

    tracing::info!("FileLogService shutting down (Priority: SYSTEM)");
    Ok(())
}
