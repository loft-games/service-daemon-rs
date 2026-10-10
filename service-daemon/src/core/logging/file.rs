use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;
use tracing_appender::non_blocking::NonBlockingBuilder;
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

/// Counts errors returned by the rolling appender's background worker.
///
/// `tracing-appender` intentionally keeps the worker thread detached from the
/// caller and does not expose its I/O errors. The file service polls this
/// counter so a failed disk write remains visible on stderr.
struct ErrorTrackingWriter<W> {
    inner: W,
    errors: Arc<AtomicUsize>,
}

impl<W> ErrorTrackingWriter<W> {
    fn new(inner: W, errors: Arc<AtomicUsize>) -> Self {
        Self { inner, errors }
    }

    fn record_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }
}

impl<W: Write> Write for ErrorTrackingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.inner.write(buf) {
            Ok(written) => Ok(written),
            Err(error) => {
                self.record_error();
                Err(error)
            }
        }
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        match self.inner.write_all(buf) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.record_error();
                Err(error)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.inner.flush() {
            Ok(()) => Ok(()),
            Err(error) => {
                self.record_error();
                Err(error)
            }
        }
    }
}

struct FileLogHealthReporter {
    last_report_at: Option<Instant>,
    last_dropped_lines: usize,
    last_io_errors: usize,
}

impl FileLogHealthReporter {
    fn new() -> Self {
        Self {
            last_report_at: None,
            last_dropped_lines: 0,
            last_io_errors: 0,
        }
    }

    fn report(&mut self, dropped_lines: usize, io_errors: usize, force: bool) {
        let changed = dropped_lines != self.last_dropped_lines || io_errors != self.last_io_errors;
        self.last_dropped_lines = dropped_lines;
        self.last_io_errors = io_errors;

        if !changed {
            return;
        }

        let can_report = force
            || self
                .last_report_at
                .is_none_or(|last| last.elapsed() >= Duration::from_secs(1));
        if can_report {
            eprintln!(
                "service-daemon file logging health: dropped_lines={dropped_lines}, io_errors={io_errors}"
            );
            self.last_report_at = Some(Instant::now());
        }
    }
}

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
/// When `FILE_LOG_CONFIG` is not set, this service stays idle until shutdown.
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
    let io_errors = Arc::new(AtomicUsize::new(0));
    let tracked_appender = ErrorTrackingWriter::new(file_appender, io_errors.clone());
    let (mut writer, guard) = NonBlockingBuilder::default()
        // Keep file I/O off the tracing producer path. Losses are surfaced by
        // the error counter and reported on stderr instead of blocking a Tokio
        // runtime worker when the disk is slow or unavailable.
        .lossy(true)
        .thread_name("service-daemon-file-log")
        .finish(tracked_appender);
    let dropped_lines = writer.error_counter();
    let mut health = FileLogHealthReporter::new();

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
                        health.report(
                            dropped_lines.dropped_lines(),
                            io_errors.load(Ordering::Relaxed),
                            false,
                        );
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        eprintln!(
                            "service-daemon file logging lagged; skipped_broadcast_events={n}"
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
    drop(guard);
    health.report(
        dropped_lines.dropped_lines(),
        io_errors.load(Ordering::Relaxed),
        true,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ErrorTrackingWriter;
    use std::io::{self, Write};
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("simulated file write failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("simulated file flush failure"))
        }
    }

    #[test]
    fn tracked_writer_counts_write_and_flush_errors() {
        let errors = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut writer = ErrorTrackingWriter::new(FailingWriter, errors.clone());

        assert!(writer.write_all(b"event").is_err());
        assert!(writer.flush().is_err());
        assert_eq!(errors.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn non_blocking_file_writer_reports_background_errors() {
        let errors = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tracked_writer = ErrorTrackingWriter::new(FailingWriter, errors.clone());
        let (mut writer, guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
            .lossy(true)
            .finish(tracked_writer);

        writer.write_all(b"event").unwrap();
        drop(guard);

        assert!(
            errors.load(Ordering::Relaxed) > 0,
            "background file writer errors should be observable"
        );
    }
}
