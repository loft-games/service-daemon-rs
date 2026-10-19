use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};
use std::{path::PathBuf, sync::mpsc::Receiver};

use chrono::Timelike;
use parking_lot::Mutex as ParkingMutex;
use tokio::sync::broadcast;
use tracing_appender::non_blocking::NonBlockingBuilder;

use crate::ServicePriority;
use crate::service;

use super::model::{LogEvent, effective_batch_size, get_log_queue};
use super::render::format_event_json;

use super::archive::{ArchiveStore, rotated_log_name};

/// Time-based log rotation strategy.
///
/// Controls how frequently the file appender rotates to a new log file.
/// Only available when the `file-logging` feature is enabled.
#[derive(Debug, Clone, Copy, Default)]
pub enum RotationPolicy {
    /// Rotate daily at UTC boundaries (default), e.g. `latest.2026-03-03.log`.
    #[default]
    Daily,
    /// Rotate hourly. Suitable for high-volume services.
    Hourly,
    /// Never rotate. Single file, relies on external log rotation tools.
    Never,
}

/// Compression strategy for files produced by time-based rotation.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum FileLogCompression {
    /// Keep rotated files as plain JSON Lines.
    None,
    /// Compress closed rotated files with gzip (default).
    #[default]
    Gzip,
}

/// Configuration for file-based log persistence.
///
/// Controls the output directory, file prefix, rotation strategy, and
/// compression strategy. Only available when the `file-logging` feature is enabled.
///
/// # Rotation Strategy
/// Writes to the configured file name, `latest.log` by default. On the first
/// write in a later UTC period, the closed file is renamed with its period
/// before the last extension, e.g. `latest.2026-10-19.log`.
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
    /// Active file name (e.g., "app.log" rotates to "app.2026-02-24.log").
    pub file_prefix: String,
    /// Time-based rotation strategy. Default: `RotationPolicy::Daily`.
    pub rotation: RotationPolicy,
    /// Compression strategy for closed rotated files. Default: gzip.
    pub compression: FileLogCompression,
}

impl FileLogConfig {
    /// Creates a new file log configuration.
    ///
    /// Uses daily rotation and gzip compression.
    ///
    /// # Arguments
    /// * `directory` - Path to the log output directory (created if missing).
    /// * `file_prefix` - Active file name, preserved exactly as supplied.
    #[must_use]
    pub fn new(directory: impl Into<String>, file_prefix: impl Into<String>) -> Self {
        Self {
            directory: directory.into(),
            file_prefix: file_prefix.into(),
            rotation: RotationPolicy::Daily,
            compression: FileLogCompression::Gzip,
        }
    }
}

impl Default for FileLogConfig {
    fn default() -> Self {
        Self {
            directory: "logs".to_string(),
            file_prefix: "latest.log".to_string(),
            rotation: RotationPolicy::Daily,
            compression: FileLogCompression::Gzip,
        }
    }
}

/// Global file log configuration, set once before the daemon starts.
/// When `None`, file logging is disabled even if the feature is compiled in.
static FILE_LOG_CONFIG: tokio::sync::OnceCell<FileLogConfig> = tokio::sync::OnceCell::const_new();

enum ArchiveCommand {
    Maintain { now: SystemTime },
}

struct ArchiveWorker {
    tx: SyncSender<ArchiveCommand>,
    cancel: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl ArchiveWorker {
    fn start(
        config: &FileLogConfig,
        active_path: Arc<ParkingMutex<Option<PathBuf>>>,
    ) -> io::Result<Self> {
        let store = ArchiveStore::new(
            PathBuf::from(&config.directory),
            config.file_prefix.clone(),
            config.rotation,
        );
        let (tx, rx) = sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = Arc::clone(&cancel);
        let stopping = Arc::new(AtomicBool::new(false));
        let thread_stopping = Arc::clone(&stopping);
        let thread_active_path = Arc::clone(&active_path);
        let join = thread::Builder::new()
            .name("service-daemon-log-archive".to_string())
            .spawn(move || {
                run_archive_worker(
                    store,
                    rx,
                    thread_cancel,
                    thread_stopping,
                    thread_active_path,
                )
            })?;
        Ok(Self {
            tx,
            cancel,
            stopping,
            join: Some(join),
        })
    }

    fn request(&self) {
        let command = ArchiveCommand::Maintain {
            now: SystemTime::now(),
        };
        match self.tx.try_send(command) {
            Ok(()) => {}
            Err(TrySendError::Disconnected(_)) => {
                eprintln!(
                    "service-daemon log archive worker disconnected; closed logs remain on disk"
                );
            }
            Err(TrySendError::Full(_)) => {
                eprintln!("service-daemon log archive queue is full; retrying later");
            }
        }
    }

    async fn shutdown(mut self) {
        self.stopping.store(true, Ordering::Release);
        // Wake an idle worker. If the slot is full its pending pass observes
        // `stopping` and performs the final scan using the current active file.
        self.request();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let finished = self.join.as_ref().is_none_or(JoinHandle::is_finished);
            if finished || Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if self.join.as_ref().is_some_and(|join| !join.is_finished()) {
            self.cancel.store(true, Ordering::Release);
            eprintln!(
                "service-daemon log archive shutdown exceeded two seconds; unfinished work remains recoverable"
            );
            // Cancellation is checked between copy chunks and receive waits.
            // Detach only after signalling it so a cancelled service future
            // cannot leave a permanently live archive thread.
            self.join.take();
        }
        if let Some(join) = self.join.take()
            && join.join().is_err()
        {
            eprintln!("service-daemon log archive worker panicked during shutdown");
        }
    }
}

impl Drop for ArchiveWorker {
    fn drop(&mut self) {
        // A cancelled service future must also stop its archive thread. The
        // receiver uses a timeout, so it exits even when no wake-up is queued.
        self.cancel.store(true, Ordering::Release);
    }
}

fn run_archive_worker(
    store: ArchiveStore,
    rx: Receiver<ArchiveCommand>,
    cancel: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    active_path: Arc<ParkingMutex<Option<PathBuf>>>,
) {
    loop {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let queued_now = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ArchiveCommand::Maintain { now }) => now,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if stopping.load(Ordering::Acquire) => {
                SystemTime::now()
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let shutdown = stopping.load(Ordering::Acquire);
        // A maintenance request may have been queued before shutdown began.
        // Use a fresh cutoff for that final pass so a stale queued timestamp
        // cannot leave the just-closed period pending until the next startup.
        let now = if shutdown {
            SystemTime::now()
        } else {
            queued_now
        };
        let active_path = active_path.lock().clone();
        if let Err(error) = store.maintain(active_path.as_deref(), now, &cancel) {
            eprintln!("service-daemon log archive maintenance failed: {error}");
        }
        if shutdown || cancel.load(Ordering::Acquire) {
            break;
        }
    }
}

fn active_log_path(config: &FileLogConfig) -> PathBuf {
    PathBuf::from(&config.directory).join(&config.file_prefix)
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum WriterPeriod {
    Daily(chrono::NaiveDate),
    Hourly(chrono::NaiveDate, u32),
    Never,
}

impl WriterPeriod {
    fn at(rotation: RotationPolicy, now: SystemTime) -> Self {
        let timestamp = chrono::DateTime::<chrono::Utc>::from(now);
        match rotation {
            RotationPolicy::Daily => Self::Daily(timestamp.date_naive()),
            RotationPolicy::Hourly => Self::Hourly(timestamp.date_naive(), timestamp.hour()),
            RotationPolicy::Never => Self::Never,
        }
    }

    fn suffix(self) -> String {
        match self {
            Self::Daily(day) => day.format("%Y-%m-%d").to_string(),
            Self::Hourly(day, hour) => format!("{}-{hour:02}", day.format("%Y-%m-%d")),
            Self::Never => String::new(),
        }
    }
}

struct RotatingFileWriter {
    directory: PathBuf,
    prefix: String,
    rotation: RotationPolicy,
    period: WriterPeriod,
    errors: Arc<AtomicUsize>,
    // Rotation closes the old handle before renaming it on Windows. If opening
    // the next active file fails, a later write retries without touching archives.
    file: Option<File>,
}

impl RotatingFileWriter {
    fn new(
        config: &FileLogConfig,
        active_path: Arc<ParkingMutex<Option<PathBuf>>>,
        errors: Arc<AtomicUsize>,
        now: SystemTime,
    ) -> io::Result<Self> {
        let path = active_log_path(config);
        fs::create_dir_all(&config.directory)?;
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let metadata = file.metadata()?;
        let period = WriterPeriod::at(
            config.rotation,
            if metadata.len() == 0 {
                now
            } else {
                metadata.modified()?
            },
        );
        *active_path.lock() = Some(path);
        let mut writer = Self {
            directory: PathBuf::from(&config.directory),
            prefix: config.file_prefix.clone(),
            rotation: config.rotation,
            period,
            errors,
            file: Some(file),
        };
        writer.rotate(now);
        Ok(writer)
    }

    fn try_rotate(&mut self, now: SystemTime) -> io::Result<()> {
        let next_period = WriterPeriod::at(self.rotation, now);
        if next_period <= self.period || next_period == WriterPeriod::Never {
            return Ok(());
        }
        let file = self.current_file()?;
        if file.metadata()?.len() == 0 {
            self.period = next_period;
            return Ok(());
        }
        file.flush()?;
        file.sync_all()?;
        let destination = self.rotation_destination()?;
        let active = self.directory.join(&self.prefix);
        drop(self.file.take());
        // Each directory/prefix has a single writer. The destination is chosen
        // without replacing an existing plain file or compressed archive.
        // On failure the next write reopens the unchanged active file.
        fs::rename(&active, &destination)?;
        self.period = next_period;
        self.file = Some(
            OpenOptions::new()
                .create_new(true)
                .append(true)
                .open(active)?,
        );
        Ok(())
    }

    fn rotation_destination(&self) -> io::Result<PathBuf> {
        let suffix = self.period.suffix();
        let mut sequence = None;
        loop {
            let name = rotated_log_name(&self.prefix, &suffix, sequence);
            let path = self.directory.join(&name);
            let gzip = self.directory.join(format!("{name}.gz"));
            let mut occupied = false;
            for candidate in [&path, &gzip] {
                match fs::symlink_metadata(candidate) {
                    Ok(metadata) if metadata.is_file() => occupied = true,
                    Ok(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            format!(
                                "log rotation destination is not a regular file: {}",
                                candidate.display()
                            ),
                        ));
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            if !occupied {
                return Ok(path);
            }
            sequence = Some(
                sequence
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("log rotation fragment sequence exhausted"))?,
            );
        }
    }

    fn current_file(&mut self) -> io::Result<&mut File> {
        if self.file.is_none() {
            self.file = Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.directory.join(&self.prefix))?,
            );
        }
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("active log file is unavailable"))
    }

    fn rotate(&mut self, now: SystemTime) {
        if let Err(error) = self.try_rotate(now) {
            self.errors.fetch_add(1, Ordering::Relaxed);
            eprintln!(
                "service-daemon file log rotation failed: {error}; retrying on the next write"
            );
        }
    }

    fn write_at(&mut self, buffer: &[u8], now: SystemTime) -> io::Result<usize> {
        self.rotate(now);
        self.current_file()?.write(buffer)
    }
}

impl Write for RotatingFileWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.write_at(buffer, SystemTime::now())
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

/// Counts errors returned by the background file writer.
///
/// The file service polls this counter so a failed disk write remains visible
/// on stderr instead of silently disabling persistence.
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
/// JSON lines (one JSON object per line), written to the configured active
/// file, `latest.log` by default. After closing a period, rotation produces
/// `latest.YYYY-MM-DD.log` or `latest.YYYY-MM-DD-HH.log`. With gzip compression,
/// `.gz` is appended asynchronously, with same-day compaction and delayed
/// repack handled by the archive worker. The active file is never archived.
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

    let active_path = Arc::new(ParkingMutex::new(None));
    let io_errors = Arc::new(AtomicUsize::new(0));
    let file_appender = match RotatingFileWriter::new(
        config,
        Arc::clone(&active_path),
        Arc::clone(&io_errors),
        SystemTime::now(),
    ) {
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

    let archive_worker = match config.compression {
        FileLogCompression::Gzip if !matches!(config.rotation, RotationPolicy::Never) => {
            match ArchiveWorker::start(config, active_path) {
                Ok(worker) => Some(worker),
                Err(error) => {
                    eprintln!("service-daemon log archive worker disabled: {error}");
                    None
                }
            }
        }
        FileLogCompression::Gzip | FileLogCompression::None => None,
    };
    let mut archive_worker = archive_worker;
    let mut archive_tick = tokio::time::interval(Duration::from_secs(1));
    archive_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    if let Some(worker) = archive_worker.as_ref() {
        worker.request();
    }

    let mut rx = get_log_queue().tx.subscribe();
    let batch_size = effective_batch_size();
    let mut buffer: Vec<Arc<LogEvent>> = Vec::with_capacity(batch_size);

    while !service_daemon::is_shutdown() {
        tokio::select! {
            biased;
            _ = service_daemon::wait_shutdown() => {
                break;
            }
            _ = archive_tick.tick(), if archive_worker.is_some() => {
                if let Some(worker) = archive_worker.as_ref() {
                    worker.request();
                }
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

    drop(guard);
    health.report(
        dropped_lines.dropped_lines(),
        io_errors.load(Ordering::Relaxed),
        true,
    );
    if let Some(worker) = archive_worker.take() {
        worker.shutdown().await;
    }
    tracing::info!("FileLogService shutting down (Priority: SYSTEM)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Write};
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "service-daemon-writer-test-{}",
                uuid::Uuid::now_v7()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn config(&self) -> FileLogConfig {
            FileLogConfig::new(self.0.to_string_lossy(), "latest.log")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn day(day: u32) -> SystemTime {
        chrono::NaiveDate::from_ymd_opt(2026, 1, day)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .into()
    }

    fn decompress(path: &Path) -> String {
        use std::io::Read;
        let mut result = String::new();
        flate2::read::MultiGzDecoder::new(File::open(path).unwrap())
            .read_to_string(&mut result)
            .unwrap();
        result
    }

    fn maintain(
        config: &FileLogConfig,
        active: &Arc<ParkingMutex<Option<PathBuf>>>,
        now: SystemTime,
    ) {
        let store = ArchiveStore::new(
            PathBuf::from(&config.directory),
            config.file_prefix.clone(),
            config.rotation,
        );
        store
            .maintain(active.lock().as_deref(), now, &AtomicBool::new(false))
            .unwrap();
    }

    #[test]
    fn current_events_use_fixed_name_until_rotation() {
        let directory = TestDirectory::new();
        let config = directory.config();
        let active = Arc::new(ParkingMutex::new(None));
        let mut writer = RotatingFileWriter::new(
            &config,
            active.clone(),
            Arc::new(AtomicUsize::new(0)),
            day(1),
        )
        .unwrap();
        writer.write_at(b"first day\n", day(1)).unwrap();
        assert_eq!(
            fs::read(directory.0.join("latest.log")).unwrap(),
            b"first day\n"
        );
        assert!(!directory.0.join("latest.2026-01-01.log").exists());

        writer.write_at(b"second day\n", day(2)).unwrap();
        assert_eq!(
            fs::read(directory.0.join("latest.log")).unwrap(),
            b"second day\n"
        );
        assert_eq!(
            fs::read(directory.0.join("latest.2026-01-01.log")).unwrap(),
            b"first day\n"
        );
        assert_eq!(*active.lock(), Some(directory.0.join("latest.log")));
    }

    #[test]
    fn restart_rotates_previous_period_before_appending() {
        let directory = TestDirectory::new();
        let config = directory.config();
        let current = directory.0.join("latest.log");
        fs::write(&current, b"previous process\n").unwrap();
        File::options()
            .write(true)
            .open(&current)
            .unwrap()
            .set_modified(day(1))
            .unwrap();
        let mut writer = RotatingFileWriter::new(
            &config,
            Arc::new(ParkingMutex::new(None)),
            Arc::new(AtomicUsize::new(0)),
            day(3),
        )
        .unwrap();
        writer.write_at(b"new process\n", day(3)).unwrap();
        assert_eq!(fs::read(&current).unwrap(), b"new process\n");
        assert_eq!(
            fs::read(directory.0.join("latest.2026-01-01.log")).unwrap(),
            b"previous process\n"
        );
        assert!(!directory.0.join("latest.2026-01-02.log").exists());
    }

    #[test]
    fn restart_within_period_appends_to_fixed_name() {
        let directory = TestDirectory::new();
        let config = directory.config();
        let current = directory.0.join("latest.log");
        fs::write(&current, b"first process\n").unwrap();
        File::options()
            .write(true)
            .open(&current)
            .unwrap()
            .set_modified(day(1))
            .unwrap();
        let mut writer = RotatingFileWriter::new(
            &config,
            Arc::new(ParkingMutex::new(None)),
            Arc::new(AtomicUsize::new(0)),
            day(1),
        )
        .unwrap();
        writer.write_at(b"second process\n", day(1)).unwrap();
        assert_eq!(
            fs::read(&current).unwrap(),
            b"first process\nsecond process\n"
        );
        assert!(!directory.0.join("latest.2026-01-01.log").exists());
    }

    #[test]
    fn rotation_preserves_existing_plain_and_gzip_archives() {
        let directory = TestDirectory::new();
        let config = directory.config();
        let plain = directory.0.join("latest.2026-01-01.log");
        let gzip = directory.0.join("latest.2026-01-01.1.log.gz");
        fs::write(&plain, b"existing plain\n").unwrap();
        fs::write(&gzip, b"existing gzip\n").unwrap();
        let mut writer = RotatingFileWriter::new(
            &config,
            Arc::new(ParkingMutex::new(None)),
            Arc::new(AtomicUsize::new(0)),
            day(1),
        )
        .unwrap();
        writer.write_at(b"new fragment\n", day(1)).unwrap();
        writer.write_at(b"next period\n", day(2)).unwrap();
        assert_eq!(fs::read(&plain).unwrap(), b"existing plain\n");
        assert_eq!(fs::read(&gzip).unwrap(), b"existing gzip\n");
        assert_eq!(
            fs::read(directory.0.join("latest.2026-01-01.2.log")).unwrap(),
            b"new fragment\n"
        );
    }

    #[test]
    fn hourly_rotation_names_the_closed_hour_and_preserves_custom_extension() {
        let directory = TestDirectory::new();
        let config = FileLogConfig {
            rotation: RotationPolicy::Hourly,
            ..FileLogConfig::new(directory.0.to_string_lossy(), "application.jsonl")
        };
        let mut writer = RotatingFileWriter::new(
            &config,
            Arc::new(ParkingMutex::new(None)),
            Arc::new(AtomicUsize::new(0)),
            day(1),
        )
        .unwrap();
        writer
            .write_at(b"first hour\n", day(1) + Duration::from_secs(3599))
            .unwrap();
        assert!(!directory.0.join("application.2026-01-01-00.jsonl").exists());
        writer
            .write_at(b"later hour\n", day(1) + Duration::from_secs(7200))
            .unwrap();
        assert_eq!(
            fs::read(directory.0.join("application.2026-01-01-00.jsonl")).unwrap(),
            b"first hour\n"
        );
        assert_eq!(
            fs::read(directory.0.join("application.jsonl")).unwrap(),
            b"later hour\n"
        );
        assert!(!directory.0.join("application.2026-01-01-01.jsonl").exists());
    }

    #[test]
    fn empty_active_file_does_not_create_an_empty_archive() {
        let directory = TestDirectory::new();
        let config = directory.config();
        let mut writer = RotatingFileWriter::new(
            &config,
            Arc::new(ParkingMutex::new(None)),
            Arc::new(AtomicUsize::new(0)),
            day(1),
        )
        .unwrap();
        writer.write_at(b"first event\n", day(3)).unwrap();
        assert_eq!(
            fs::read(directory.0.join("latest.log")).unwrap(),
            b"first event\n"
        );
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn windows_sharing_violation_preserves_active_file_and_retries_rotation() {
        use std::os::windows::fs::OpenOptionsExt;

        let directory = TestDirectory::new();
        let config = directory.config();
        let active = Arc::new(ParkingMutex::new(None));
        let errors = Arc::new(AtomicUsize::new(0));
        let mut writer =
            RotatingFileWriter::new(&config, active.clone(), errors.clone(), day(1)).unwrap();
        writer.write_at(b"first\n", day(1)).unwrap();
        // Permit reads/writes but deny delete sharing, as a log reader may do.
        let reader = OpenOptions::new()
            .read(true)
            .share_mode(0x0000_0003)
            .open(directory.0.join("latest.log"))
            .unwrap();
        writer.write_at(b"rename blocked\n", day(2)).unwrap();
        assert_eq!(errors.load(Ordering::Relaxed), 1);
        maintain(&config, &active, day(3));
        assert_eq!(
            fs::read(directory.0.join("latest.log")).unwrap(),
            b"first\nrename blocked\n"
        );
        assert!(!directory.0.join("latest.2026-01-01.log.gz").exists());

        drop(reader);
        writer.write_at(b"recovered\n", day(2)).unwrap();
        maintain(&config, &active, day(3));
        assert_eq!(
            decompress(&directory.0.join("latest.2026-01-01.log.gz")),
            "first\nrename blocked\n"
        );
        assert_eq!(
            fs::read(directory.0.join("latest.log")).unwrap(),
            b"recovered\n"
        );
    }

    #[test]
    fn idle_period_boundary_preserves_actual_open_file() {
        let directory = TestDirectory::new();
        let config = directory.config();
        let active = Arc::new(ParkingMutex::new(None));
        let mut writer = RotatingFileWriter::new(
            &config,
            active.clone(),
            Arc::new(AtomicUsize::new(0)),
            day(1),
        )
        .unwrap();
        writer.write_at(b"before boundary\n", day(1)).unwrap();
        maintain(&config, &active, day(2));
        let old = active_log_path(&config);
        assert_eq!(*active.lock(), Some(old.clone()));
        assert_eq!(fs::read_to_string(old).unwrap(), "before boundary\n");
        assert!(!directory.0.join("latest.2026-01-01.log.gz").exists());
    }

    #[test]
    fn successful_rotation_closes_old_file_before_archive_handoff() {
        let directory = TestDirectory::new();
        let config = directory.config();
        let active = Arc::new(ParkingMutex::new(None));
        let mut writer = RotatingFileWriter::new(
            &config,
            active.clone(),
            Arc::new(AtomicUsize::new(0)),
            day(1),
        )
        .unwrap();
        writer.write_at(b"first\n", day(1)).unwrap();
        writer.write_at(b"second\n", day(2)).unwrap();
        assert_eq!(*active.lock(), Some(active_log_path(&config)));
        maintain(&config, &active, day(3));
        assert!(!directory.0.join("latest.2026-01-01.log").exists());
        assert_eq!(
            decompress(&directory.0.join("latest.2026-01-01.log.gz")),
            "first\n"
        );
        // The still-open second file is protected even though the maintenance
        // clock has advanced past its period.
        writer.write_at(b"still open\n", day(2)).unwrap();
        assert_eq!(
            fs::read_to_string(active_log_path(&config)).unwrap(),
            "second\nstill open\n"
        );
    }

    #[test]
    fn rotation_destination_failure_keeps_old_file_usable_and_protected() {
        let directory = TestDirectory::new();
        let config = directory.config();
        let active = Arc::new(ParkingMutex::new(None));
        let errors = Arc::new(AtomicUsize::new(0));
        let mut writer =
            RotatingFileWriter::new(&config, active.clone(), errors.clone(), day(1)).unwrap();
        writer.write_at(b"first\n", day(1)).unwrap();
        let blocked = directory.0.join("latest.2026-01-01.log");
        fs::create_dir(&blocked).unwrap();
        writer.write_at(b"fallback\n", day(2)).unwrap();
        assert_eq!(errors.load(Ordering::Relaxed), 1);
        assert_eq!(*active.lock(), Some(active_log_path(&config)));
        maintain(&config, &active, day(3));
        assert_eq!(
            fs::read_to_string(active_log_path(&config)).unwrap(),
            "first\nfallback\n"
        );
        fs::remove_dir(blocked).unwrap();
        writer.write_at(b"recovered\n", day(2)).unwrap();
        maintain(&config, &active, day(3));
        assert_eq!(
            decompress(&directory.0.join("latest.2026-01-01.log.gz")),
            "first\nfallback\n"
        );
    }

    #[test]
    fn backwards_clock_does_not_reopen_an_archived_period() {
        let directory = TestDirectory::new();
        let config = directory.config();
        let active = Arc::new(ParkingMutex::new(None));
        let mut writer = RotatingFileWriter::new(
            &config,
            active.clone(),
            Arc::new(AtomicUsize::new(0)),
            day(1),
        )
        .unwrap();
        writer.write_at(b"first\n", day(1)).unwrap();
        writer.write_at(b"second\n", day(2)).unwrap();
        maintain(&config, &active, day(3));
        writer.write_at(b"clock moved back\n", day(1)).unwrap();
        assert_eq!(*active.lock(), Some(active_log_path(&config)));
        assert!(!directory.0.join("latest.2026-01-01.log").exists());
        assert_eq!(
            fs::read_to_string(active_log_path(&config)).unwrap(),
            "second\nclock moved back\n"
        );
    }

    #[tokio::test]
    async fn full_archive_queue_preserves_source_and_shutdown_retries_current_state() {
        let directory = TestDirectory::new();
        let config = directory.config();
        let active = Arc::new(ParkingMutex::new(None));
        let mut writer = RotatingFileWriter::new(
            &config,
            active.clone(),
            Arc::new(AtomicUsize::new(0)),
            day(1),
        )
        .unwrap();
        writer.write_at(b"old data\n", day(1)).unwrap();

        // Keep the receiver idle so queue saturation is deterministic.
        let (tx, rx) = sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let stopping = Arc::new(AtomicBool::new(false));
        let mut worker = ArchiveWorker {
            tx,
            cancel: cancel.clone(),
            stopping: stopping.clone(),
            join: None,
        };
        worker.request();
        writer.write_at(b"active data\n", day(2)).unwrap();
        worker.request();
        assert!(directory.0.join("latest.2026-01-01.log").is_file());

        let store = ArchiveStore::new(
            PathBuf::from(&config.directory),
            config.file_prefix.clone(),
            config.rotation,
        );
        worker.join = Some(thread::spawn(move || {
            run_archive_worker(store, rx, cancel, stopping, active)
        }));
        worker.shutdown().await;
        assert_eq!(
            decompress(&directory.0.join("latest.2026-01-01.log.gz")),
            "old data\n"
        );
        assert_eq!(
            fs::read_to_string(active_log_path(&config)).unwrap(),
            "active data\n"
        );
    }

    #[test]
    fn shutdown_refreshes_cutoff_for_a_stale_queued_pass() {
        let directory = TestDirectory::new();
        let config = directory.config();
        let active = Arc::new(ParkingMutex::new(None));
        let mut writer = RotatingFileWriter::new(
            &config,
            active.clone(),
            Arc::new(AtomicUsize::new(0)),
            day(1),
        )
        .unwrap();
        writer.write_at(b"old data\n", day(1)).unwrap();
        writer.write_at(b"active data\n", day(2)).unwrap();
        drop(writer);

        let (tx, rx) = sync_channel(1);
        tx.send(ArchiveCommand::Maintain { now: day(1) })
            .expect("queue stale pass");
        let cancel = Arc::new(AtomicBool::new(false));
        let stopping = Arc::new(AtomicBool::new(true));
        let store = ArchiveStore::new(
            PathBuf::from(&config.directory),
            config.file_prefix.clone(),
            config.rotation,
        );
        let join = thread::spawn(move || run_archive_worker(store, rx, cancel, stopping, active));
        join.join().expect("archive worker");

        assert_eq!(
            decompress(&directory.0.join("latest.2026-01-01.log.gz")),
            "old data\n"
        );
        assert_eq!(
            fs::read_to_string(active_log_path(&config)).unwrap(),
            "active data\n"
        );
    }

    #[test]
    fn dropping_worker_cancels_idle_thread() {
        let directory = TestDirectory::new();
        let mut worker =
            ArchiveWorker::start(&directory.config(), Arc::new(ParkingMutex::new(None))).unwrap();
        let cancel = worker.cancel.clone();
        let join = worker.join.take().unwrap();
        drop(worker);
        assert!(cancel.load(Ordering::Acquire));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !join.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(join.is_finished(), "idle archive thread did not stop");
        join.join().unwrap();
    }

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
