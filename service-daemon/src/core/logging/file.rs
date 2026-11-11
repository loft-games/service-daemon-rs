use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};
use std::{path::PathBuf, sync::mpsc::Receiver};

use parking_lot::Mutex as ParkingMutex;
use tokio::sync::broadcast;
use tracing_appender::non_blocking::NonBlockingBuilder;

use crate::ServicePriority;
use crate::service;

use super::model::{LogEvent, effective_batch_size, get_file_log_queue};
use super::render::format_event_json;

use super::archive::{ArchiveStore, SCAN_INTERVAL};

use super::file_config::{FileLogCompression, FileLogConfig, FileLogConfigError, RotationPolicy};

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
        let store = ArchiveStore::new(config)?;
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
    mut store: ArchiveStore,
    rx: Receiver<ArchiveCommand>,
    cancel: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    _active_path: Arc<ParkingMutex<Option<PathBuf>>>,
) {
    let mut busy = false;
    loop {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let command = if busy {
            rx.try_recv().ok()
        } else {
            rx.recv_timeout(Duration::from_millis(100)).ok()
        };
        let mut now = SystemTime::now();
        if let Some(ArchiveCommand::Maintain { now: requested }) = command {
            now = requested;
            store.request();
        }
        if stopping.load(Ordering::Acquire) {
            now = SystemTime::now();
        }
        match store.step(now, &cancel) {
            Ok(work) => busy = work,
            Err(error) => {
                eprintln!("service-daemon log archive maintenance failed: {error}");
                busy = true;
            }
        }
        if !busy && stopping.load(Ordering::Acquire) {
            break;
        }
    }
}

use super::rotation::RotatingFileWriter;

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
    if let Err(error) = try_enable_file_logging(config) {
        eprintln!("service-daemon {error}");
    }
}

/// Configures file logging once, returning validation and duplicate-setup errors.
pub fn try_enable_file_logging(config: FileLogConfig) -> Result<(), FileLogConfigError> {
    config.validate()?;
    FILE_LOG_CONFIG
        .set(config)
        .map_err(|_| FileLogConfigError::AlreadyEnabled)
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
/// `latest.segment.<local-start-time>.<sequence>.log`. Background gzip uses
/// independently configured fragment, daily and monthly levels. Daily grouping
/// follows the starting local date; monthly volumes are independently readable.
/// The active file is never archived.
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
    let mut file_appender = match RotatingFileWriter::new(
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
    if let Some(worker) = archive_worker.as_ref() {
        let tx = worker.tx.clone();
        file_appender.set_rotation_notifier(move || {
            let _ = tx.try_send(ArchiveCommand::Maintain {
                now: SystemTime::now(),
            });
        });
    }
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

    let mut archive_worker = archive_worker;
    let mut archive_tick = tokio::time::interval(SCAN_INTERVAL);
    archive_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    if let Some(worker) = archive_worker.as_ref() {
        worker.request();
    }

    let mut rx = get_file_log_queue().tx.subscribe();
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
                                let mut json_line = format_event_json(&event);
                                json_line.push('\n');
                                let _ = writer.write_all(json_line.as_bytes());
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
            let mut json_line = format_event_json(&event);
            json_line.push('\n');
            let _ = writer.write_all(json_line.as_bytes());
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
    use std::{fs, io::Read};

    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn fixture() -> (TestDirectory, FileLogConfig) {
        let directory =
            TestDirectory(std::env::temp_dir().join(format!("sd-worker-{}", uuid::Uuid::now_v7())));
        fs::create_dir_all(directory.path()).unwrap();
        let mut config = FileLogConfig::new(directory.path().to_string_lossy(), "latest.log");
        config.archive.daily_start_jitter = Duration::ZERO;
        config.archive.monthly_start_jitter = Duration::ZERO;
        config.archive.monthly_volume_jitter = Duration::ZERO;
        (directory, config)
    }
    fn decompress(path: &std::path::Path) -> Vec<u8> {
        let mut bytes = Vec::new();
        flate2::read::MultiGzDecoder::new(fs::File::open(path).unwrap())
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    }

    #[tokio::test]
    async fn full_wakeup_queue_preserves_sources_and_shutdown_rescans() {
        let (directory, config) = fixture();
        let stamp = chrono::Local::now().format("%Y-%m-%d-%H-%M-%S");
        let name = format!("latest.segment.{stamp}.1.log");
        fs::write(directory.path().join(&name), b"closed\n").unwrap();
        fs::write(directory.path().join("latest.log"), b"active\n").unwrap();
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
        worker.request();
        assert!(directory.path().join(&name).is_file());
        let store = ArchiveStore::new(&config).unwrap();
        worker.join = Some(thread::spawn(move || {
            run_archive_worker(
                store,
                rx,
                cancel,
                stopping,
                Arc::new(ParkingMutex::new(None)),
            )
        }));
        worker.shutdown().await;
        assert_eq!(
            decompress(&directory.path().join(format!("{name}.gz"))),
            b"closed\n"
        );
        assert_eq!(
            fs::read(directory.path().join("latest.log")).unwrap(),
            b"active\n"
        );
    }

    #[test]
    fn shutdown_refreshes_a_stale_queued_cutoff() {
        let (directory, config) = fixture();
        fs::write(directory.path().join("latest.2020-01-01.log"), b"closed\n").unwrap();
        let (tx, rx) = sync_channel(1);
        tx.send(ArchiveCommand::Maintain {
            now: SystemTime::UNIX_EPOCH,
        })
        .unwrap();
        let store = ArchiveStore::new(&config).unwrap();
        run_archive_worker(
            store,
            rx,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(true)),
            Arc::new(ParkingMutex::new(None)),
        );
        assert_eq!(
            decompress(
                &directory
                    .path()
                    .join("latest.monthly.2020-01.part0001.log.gz")
            ),
            b"closed\n"
        );
    }

    #[test]
    fn dropping_worker_cancels_the_idle_thread() {
        let (_directory, config) = fixture();
        let mut worker = ArchiveWorker::start(&config, Arc::new(ParkingMutex::new(None))).unwrap();
        let cancel = worker.cancel.clone();
        let join = worker.join.take().unwrap();
        drop(worker);
        assert!(cancel.load(Ordering::Acquire));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !join.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(join.is_finished());
        join.join().unwrap();
    }

    #[tokio::test]
    async fn shutdown_retains_future_scheduled_archive_instead_of_bypassing_jitter() {
        let (directory, mut config) = fixture();
        config.archive.daily_start_jitter = Duration::from_secs(1800);
        let name = "latest.2020-01-01.log";
        fs::write(directory.path().join(name), b"closed\n").unwrap();
        let worker = ArchiveWorker::start(&config, Arc::new(ParkingMutex::new(None))).unwrap();
        worker.shutdown().await;
        assert_eq!(
            decompress(&directory.path().join(format!("{name}.gz"))),
            b"closed\n"
        );
        assert!(
            !directory
                .path()
                .join("latest.monthly.2020-01.part0001.log.gz")
                .exists()
        );
        let mut store = ArchiveStore::new(&config).unwrap();
        assert!(
            !store
                .step(SystemTime::now(), &AtomicBool::new(false))
                .unwrap()
        );
        assert!(directory.path().join(format!("{name}.gz")).exists());
    }

    #[tokio::test]
    async fn shutdown_cancels_unfinished_work_after_the_two_second_window() {
        let (tx, rx) = sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let stopped = Arc::new(AtomicBool::new(false));
        let thread_cancel = cancel.clone();
        let thread_stopped = stopped.clone();
        let join = thread::spawn(move || {
            let _receiver = rx;
            while !thread_cancel.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(5));
            }
            thread_stopped.store(true, Ordering::Release);
        });
        let worker = ArchiveWorker {
            tx,
            cancel: cancel.clone(),
            stopping: Arc::new(AtomicBool::new(false)),
            join: Some(join),
        };
        let start = Instant::now();
        worker.shutdown().await;
        assert!(start.elapsed() >= Duration::from_secs(2));
        assert!(start.elapsed() < Duration::from_secs(4));
        assert!(cancel.load(Ordering::Acquire));
        let deadline = Instant::now() + Duration::from_secs(1);
        while !stopped.load(Ordering::Acquire) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(stopped.load(Ordering::Acquire));
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
