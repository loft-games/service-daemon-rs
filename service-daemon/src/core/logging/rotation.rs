//! Single-owner writer: complete records, byte accounting, and durable deadlines.
use super::{
    archive::rotated_log_name,
    file_config::{FileLogConfig, RotationPolicy},
};
use chrono::{DateTime, Local, Timelike};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone, Serialize, Deserialize)]
struct ActiveState {
    id: String,
    #[serde(default = "first_sequence")]
    sequence: u64,
    started: SystemTime,
    deadline: Option<SystemTime>,
    date: String,
    stamp: String,
    #[serde(default)]
    pending: Option<String>,
    #[serde(default)]
    next: Option<Box<ActiveState>>,
}

pub(super) fn state_path(config: &FileLogConfig) -> PathBuf {
    Path::new(&config.directory).join(format!(".{}.active.json", config.file_prefix))
}

pub(super) fn active_date(config: &FileLogConfig) -> io::Result<Option<String>> {
    let active = Path::new(&config.directory).join(&config.file_prefix);
    match fs::metadata(active) {
        Ok(metadata) if metadata.len() == 0 => return Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        _ => {}
    }
    match fs::read(state_path(config)) {
        Ok(bytes) => Ok(Some(serde_json::from_slice::<ActiveState>(&bytes)?.date)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// A pending rename belongs to the writer until its new active state is durable.
pub(super) fn pending_fragment(config: &FileLogConfig) -> io::Result<Option<String>> {
    match fs::read(state_path(config)) {
        Ok(bytes) => Ok(serde_json::from_slice::<ActiveState>(&bytes)?.pending),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(super) fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub(super) fn save_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let temporary = path.with_file_name(format!(
        "{}.tmp",
        path.file_name()
            .ok_or_else(|| io::Error::other("missing filename"))?
            .to_string_lossy()
    ));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    serde_json::to_writer(&mut file, value)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)?;
    sync_directory(
        path.parent()
            .ok_or_else(|| io::Error::other("missing parent directory"))?,
    )
}

fn sampled_interval(base: Duration, jitter: f64) -> Duration {
    Duration::from_secs_f64(
        base.as_secs_f64() * (1.0 + (rand::random::<f64>() * 2.0 - 1.0) * jitter),
    )
    .max(Duration::from_nanos(1))
}

fn first_sequence() -> u64 {
    1
}

impl ActiveState {
    fn validate(&self) -> io::Result<()> {
        if self.id.is_empty()
            || self.sequence == 0
            || chrono::NaiveDate::parse_from_str(&self.date, "%Y-%m-%d").is_err()
            || chrono::NaiveDateTime::parse_from_str(&self.stamp, "%Y-%m-%d-%H-%M-%S").is_err()
            || self.pending.is_some() != self.next.is_some()
            || self.pending.as_ref().is_some_and(|name| {
                name.is_empty()
                    || name.contains(['/', '\\', ':', '\0'])
                    || name == "."
                    || name == ".."
            })
        {
            return Err(io::Error::other(
                "invalid active log state; original files retained",
            ));
        }
        if let Some(next) = &self.next {
            next.validate()?;
        }
        Ok(())
    }
    fn new(config: &FileLogConfig, now: SystemTime) -> io::Result<Self> {
        let local: DateTime<Local> = now.into();
        let deadline = match config.rotation {
            RotationPolicy::Interval(base) => Some(
                now.checked_add(sampled_interval(base, config.rotation_jitter))
                    .ok_or_else(|| io::Error::other("rotation deadline overflow"))?,
            ),
            _ => None,
        };
        Ok(Self {
            id: uuid::Uuid::now_v7().to_string(),
            sequence: 1,
            started: now,
            deadline,
            date: local.format("%Y-%m-%d").to_string(),
            stamp: local.format("%Y-%m-%d-%H-%M-%S").to_string(),
            pending: None,
            next: None,
        })
    }
}

pub(super) struct RotatingFileWriter {
    config: FileLogConfig,
    state: ActiveState,
    deadline: Option<Instant>,
    bytes: u64,
    file: Option<File>,
    errors: Arc<AtomicUsize>,
    notifier: Option<Box<dyn Fn() + Send>>,
    rollback: Option<u64>,
    state_dirty: bool,
}
impl RotatingFileWriter {
    pub(super) fn new(
        config: &FileLogConfig,
        active_path: Arc<Mutex<Option<PathBuf>>>,
        errors: Arc<AtomicUsize>,
        now: SystemTime,
    ) -> io::Result<Self> {
        config.validate().map_err(io::Error::other)?;
        fs::create_dir_all(&config.directory)?;
        let path = Path::new(&config.directory).join(&config.file_prefix);
        let persisted = state_path(config);
        let mut state = match fs::read(&persisted) {
            Ok(data) => serde_json::from_slice::<ActiveState>(&data)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let started = match fs::metadata(&path) {
                    Ok(meta) if meta.len() > 0 => meta.modified()?,
                    Ok(_) => now,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => now,
                    Err(error) => return Err(error),
                };
                ActiveState::new(config, started)?
            }
            Err(error) => return Err(error),
        };
        state.validate()?;
        if let Some(target) = &state.pending {
            if Path::new(&config.directory).join(target).is_file() {
                state = *state
                    .next
                    .take()
                    .ok_or_else(|| io::Error::other("rotation journal lacks next state"))?;
            } else {
                state.pending = None;
                state.next = None;
            }
        }
        if state.deadline.is_none()
            && let RotationPolicy::Interval(base) = config.rotation
        {
            state.deadline = Some(
                state
                    .started
                    .checked_add(sampled_interval(base, config.rotation_jitter))
                    .ok_or_else(|| io::Error::other("rotation deadline overflow"))?,
            );
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let bytes = file.metadata()?.len();
        save_json(&persisted, &state)?;
        let deadline = Self::monotonic_deadline(config, &state, now);
        *active_path.lock() = Some(path);
        let mut writer = Self {
            config: config.clone(),
            state,
            deadline,
            bytes,
            file: Some(file),
            errors,
            notifier: None,
            rollback: None,
            state_dirty: false,
        };
        if writer.time_due(now) && bytes > 0 {
            writer.rotate(now)?;
        }
        Ok(writer)
    }
    pub(super) fn set_rotation_notifier(&mut self, notifier: impl Fn() + Send + 'static) {
        self.notifier = Some(Box::new(notifier));
    }
    fn monotonic_deadline(
        config: &FileLogConfig,
        state: &ActiveState,
        now: SystemTime,
    ) -> Option<Instant> {
        let RotationPolicy::Interval(base) = config.rotation else {
            return None;
        };
        let maximum = Duration::from_secs_f64(base.as_secs_f64() * (1.0 + config.rotation_jitter));
        state.deadline.map(|deadline| {
            Instant::now()
                + deadline
                    .duration_since(now)
                    .unwrap_or_default()
                    .min(maximum)
        })
    }
    fn time_due(&self, now: SystemTime) -> bool {
        match self.config.rotation {
            RotationPolicy::Never => false,
            RotationPolicy::Interval(_) => {
                self.deadline.is_some_and(|value| Instant::now() >= value)
            }
            policy => {
                let current: DateTime<Local> = now.into();
                let initial: DateTime<Local> = self.state.started.into();
                current.date_naive() > initial.date_naive()
                    || (policy == RotationPolicy::Hourly
                        && current.date_naive() == initial.date_naive()
                        && current.hour() > initial.hour())
            }
        }
    }
    fn target(&self) -> io::Result<(PathBuf, u64)> {
        let directory = Path::new(&self.config.directory);
        for sequence in self.state.sequence.. {
            let suffix = format!("segment.{}.{}", self.state.stamp, sequence);
            let path = directory.join(rotated_log_name(&self.config.file_prefix, &suffix, None));
            if !path.exists()
                && !path
                    .with_file_name(format!(
                        "{}.gz",
                        path.file_name()
                            .ok_or_else(|| io::Error::other("missing filename"))?
                            .to_string_lossy()
                    ))
                    .exists()
            {
                return Ok((path, sequence));
            }
        }
        Err(io::Error::other("fragment sequence exhausted"))
    }
    fn rotate(&mut self, now: SystemTime) -> io::Result<()> {
        if self.bytes == 0 {
            return Ok(());
        }
        let (target, sequence) = self.target()?;
        let mut next = ActiveState::new(&self.config, now)?;
        next.sequence = sequence
            .checked_add(1)
            .ok_or_else(|| io::Error::other("fragment sequence exhausted"))?;
        self.state.pending = Some(
            target
                .file_name()
                .ok_or_else(|| io::Error::other("missing target filename"))?
                .to_string_lossy()
                .into_owned(),
        );
        self.state.next = Some(Box::new(next.clone()));
        save_json(&state_path(&self.config), &self.state)?;
        if let Some(file) = &mut self.file {
            file.flush()?;
            file.sync_all()?;
        }
        drop(self.file.take());
        let active = Path::new(&self.config.directory).join(&self.config.file_prefix);
        if let Err(error) = fs::rename(&active, &target) {
            self.file = Some(OpenOptions::new().append(true).open(&active)?);
            self.state.pending = None;
            self.state.next = None;
            save_json(&state_path(&self.config), &self.state)?;
            return Err(error);
        }
        sync_directory(Path::new(&self.config.directory))?;
        self.state = next;
        self.state_dirty = true;
        self.bytes = 0;
        self.deadline = Self::monotonic_deadline(&self.config, &self.state, now);
        self.file = Some(
            OpenOptions::new()
                .create_new(true)
                .append(true)
                .open(active)?,
        );
        save_json(&state_path(&self.config), &self.state)?;
        self.state_dirty = false;
        if let Some(notify) = &self.notifier {
            notify();
        }
        Ok(())
    }
    pub(super) fn write_at(&mut self, buffer: &[u8], now: SystemTime) -> io::Result<usize> {
        if self.file.is_none() {
            if let Some(target) = &self.state.pending
                && Path::new(&self.config.directory).join(target).exists()
            {
                self.state = *self
                    .state
                    .next
                    .take()
                    .ok_or_else(|| io::Error::other("rotation next state missing"))?;
            }
            self.state.pending = None;
            self.state.next = None;
            let active = Path::new(&self.config.directory).join(&self.config.file_prefix);
            let file = OpenOptions::new().create(true).append(true).open(active)?;
            self.bytes = file.metadata()?.len();
            self.file = Some(file);
            self.deadline = Self::monotonic_deadline(&self.config, &self.state, now);
            self.state_dirty = true;
        }
        if self.state_dirty {
            save_json(&state_path(&self.config), &self.state)?;
            self.state_dirty = false;
            if let Some(notify) = &self.notifier {
                notify();
            }
        }
        if let Some(length) = self.rollback {
            self.file
                .as_mut()
                .ok_or_else(|| io::Error::other("active file unavailable"))?
                .set_len(length)?;
            self.bytes = length;
            self.rollback = None;
        }
        let size_due = self
            .config
            .max_file_size
            .is_some_and(|limit| self.bytes.saturating_add(buffer.len() as u64) > limit);
        if self.config.rotation != RotationPolicy::Never
            && self.bytes > 0
            && (self.time_due(now) || size_due)
        {
            // A failed rotation must never append to an archived or missing handle.
            if let Err(error) = self.rotate(now) {
                self.errors.fetch_add(1, Ordering::Relaxed);
                eprintln!("service-daemon file log rotation failed: {error}");
                return Err(error);
            }
        }
        let file = self.file.as_mut().ok_or_else(|| {
            io::Error::other("active file unavailable; restart writer to recover rotation")
        })?;
        let start = self.bytes;
        match write_record(file, buffer, &mut self.bytes) {
            Ok(count) => Ok(count),
            Err(error) => {
                // The sink is best effort, but a failed record must not corrupt
                // the following record. Retry truncation before another write.
                self.rollback = Some(start);
                if file.set_len(start).is_ok() {
                    self.bytes = start;
                    self.rollback = None;
                }
                Err(error)
            }
        }
    }
}

fn write_record(writer: &mut impl Write, buffer: &[u8], bytes: &mut u64) -> io::Result<usize> {
    let mut offset = 0;
    while offset < buffer.len() {
        match writer.write(&buffer[offset..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "active log write returned zero",
                ));
            }
            Ok(count) => {
                offset += count;
                *bytes += count as u64;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(offset)
}
impl Write for RotatingFileWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.write_at(buffer, SystemTime::now())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("active file unavailable"))?
            .flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> FileLogConfig {
        let dir = std::env::temp_dir().join(format!("sd-rotation-{}", uuid::Uuid::now_v7()));
        FileLogConfig {
            max_file_size: Some(8),
            rotation_jitter: 0.0,
            ..FileLogConfig::new(dir.to_string_lossy(), "latest.log")
        }
    }
    fn writer(config: &FileLogConfig) -> RotatingFileWriter {
        RotatingFileWriter::new(
            config,
            Arc::new(Mutex::new(None)),
            Arc::new(AtomicUsize::new(0)),
            SystemTime::now(),
        )
        .unwrap()
    }
    #[test]
    fn size_rotation_preserves_complete_lines_and_counts_newlines() {
        let config = config();
        let mut writer = writer(&config);
        writer.write_all(b"1234567\n").unwrap();
        writer.write_all(b"next\n").unwrap();
        assert_eq!(writer.bytes, 5);
        let files: Vec<_> = fs::read_dir(&config.directory)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".segment."))
            .collect();
        assert_eq!(files.len(), 1);
        assert_eq!(fs::read(files[0].path()).unwrap(), b"1234567\n");
        assert_eq!(
            fs::read(Path::new(&config.directory).join("latest.log")).unwrap(),
            b"next\n"
        );
        drop(writer);
        fs::remove_dir_all(&config.directory).unwrap();
    }
    #[test]
    fn oversized_record_does_not_create_empty_fragment() {
        let config = config();
        let mut writer = writer(&config);
        writer.write_all(b"much longer than eight\n").unwrap();
        assert_eq!(fs::read_dir(&config.directory).unwrap().count(), 2);
        writer.write_all(b"next\n").unwrap();
        assert_eq!(writer.bytes, 5);
        drop(writer);
        fs::remove_dir_all(&config.directory).unwrap();
    }
    #[test]
    fn restart_preserves_sampled_deadline_and_size() {
        let config = config();
        let mut first = writer(&config);
        first.write_all(b"abc\n").unwrap();
        let deadline = first.state.deadline;
        let id = first.state.id.clone();
        drop(first);
        let second = writer(&config);
        assert_eq!(second.bytes, 4);
        assert_eq!(second.state.deadline, deadline);
        assert_eq!(second.state.id, id);
        drop(second);
        fs::remove_dir_all(&config.directory).unwrap();
    }
    #[test]
    fn interval_jitter_is_bounded() {
        for _ in 0..1000 {
            let duration = sampled_interval(Duration::from_secs(28800), 0.05);
            assert!(duration >= Duration::from_secs(27360));
            assert!(duration <= Duration::from_secs(30240));
        }
    }
    #[test]
    fn never_disables_size_rotation() {
        let mut config = config();
        config.rotation = RotationPolicy::Never;
        let mut writer = writer(&config);
        writer.write_all(b"1234567\n").unwrap();
        writer.write_all(b"1234567\n").unwrap();
        assert_eq!(writer.bytes, 16);
        drop(writer);
        fs::remove_dir_all(&config.directory).unwrap();
    }

    #[test]
    fn simultaneous_time_and_size_conditions_rotate_once_and_reset_the_timer() {
        let config = config();
        let mut writer = writer(&config);
        writer.write_all(b"1234567\n").unwrap();
        let before = writer.state.id.clone();
        writer.deadline = Some(Instant::now() - Duration::from_secs(1));
        writer.write_all(b"second\n").unwrap();
        assert_ne!(writer.state.id, before);
        assert_eq!(writer.state.sequence, 2);
        assert!(writer.deadline.unwrap() > Instant::now() + Duration::from_secs(28790));
        assert_eq!(writer.bytes, 7);
        drop(writer);
        fs::remove_dir_all(&config.directory).unwrap();
    }

    #[test]
    fn size_rotation_resets_the_deadline_and_sequence_survives_source_cleanup() {
        let config = config();
        let mut first = writer(&config);
        first.write_all(b"1234567\n").unwrap();
        first.deadline = Some(Instant::now() + Duration::from_secs(3));
        first.write_all(b"abcdefgh\n").unwrap();
        assert!(first.deadline.unwrap() > Instant::now() + Duration::from_secs(28790));
        let target = first.target().unwrap().0;
        drop(first);
        for entry in fs::read_dir(&config.directory).unwrap().flatten() {
            if entry.file_name().to_string_lossy().contains(".segment.") {
                fs::remove_file(entry.path()).unwrap();
            }
        }
        let mut second = writer(&config);
        assert_eq!(second.target().unwrap().0, target);
        second.write_all(b"next\n").unwrap();
        assert_eq!(second.state.sequence, 3);
        drop(second);
        fs::remove_dir_all(&config.directory).unwrap();
    }

    #[test]
    fn restart_recovers_rotation_journal_on_both_sides_of_rename() {
        for renamed in [false, true] {
            let config = config();
            let mut first = writer(&config);
            first.write_all(b"before\n").unwrap();
            let old = first.state.id.clone();
            let next = ActiveState::new(&config, SystemTime::now()).unwrap();
            let next_id = next.id.clone();
            let target = first.target().unwrap().0;
            first.state.pending = Some(target.file_name().unwrap().to_str().unwrap().into());
            first.state.next = Some(Box::new(next));
            save_json(&state_path(&config), &first.state).unwrap();
            drop(first);
            if renamed {
                fs::rename(Path::new(&config.directory).join("latest.log"), target).unwrap();
            }
            let recovered = writer(&config);
            assert_eq!(recovered.state.id, if renamed { next_id } else { old });
            assert_eq!(recovered.bytes, if renamed { 0 } else { 7 });
            assert!(recovered.state.pending.is_none());
            drop(recovered);
            fs::remove_dir_all(&config.directory).unwrap();
        }
    }

    #[test]
    fn failed_journal_write_preserves_input_and_a_later_write_retries_rotation() {
        let config = config();
        let mut writer = writer(&config);
        writer.write_all(b"1234567\n").unwrap();
        let temporary = state_path(&config).with_file_name(".latest.log.active.json.tmp");
        fs::create_dir(&temporary).unwrap();
        assert!(writer.write_all(b"next\n").is_err());
        assert_eq!(
            fs::read(Path::new(&config.directory).join("latest.log")).unwrap(),
            b"1234567\n"
        );
        fs::remove_dir(temporary).unwrap();
        writer.write_all(b"next\n").unwrap();
        assert_eq!(writer.bytes, 5);
        drop(writer);
        fs::remove_dir_all(&config.directory).unwrap();
    }

    #[test]
    fn partial_writes_and_interruptions_account_only_successful_bytes() {
        struct Partial {
            bytes: Vec<u8>,
            calls: usize,
            fail: bool,
        }
        impl Write for Partial {
            fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
                self.calls += 1;
                if self.calls == 1 {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                if self.fail && self.calls == 3 {
                    return Err(io::ErrorKind::StorageFull.into());
                }
                let count = buffer.len().min(2);
                self.bytes.extend_from_slice(&buffer[..count]);
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        for fail in [false, true] {
            let mut writer = Partial {
                bytes: Vec::new(),
                calls: 0,
                fail,
            };
            let mut bytes = 10;
            let result = write_record(&mut writer, b"record\n", &mut bytes);
            assert_eq!(bytes, 10 + writer.bytes.len() as u64);
            assert_eq!(result.is_err(), fail);
            if !fail {
                assert_eq!(writer.bytes, b"record\n");
            }
        }
    }

    #[test]
    fn interval_uses_monotonic_time_and_bounds_clock_changes_on_restart() {
        let config = config();
        let mut writer = writer(&config);
        assert!(!writer.time_due(SystemTime::now() + Duration::from_secs(365 * 86400)));
        writer.deadline = Some(Instant::now() - Duration::from_secs(1));
        assert!(writer.time_due(SystemTime::UNIX_EPOCH));
        let restored =
            RotatingFileWriter::monotonic_deadline(&config, &writer.state, SystemTime::UNIX_EPOCH)
                .unwrap();
        assert!(restored <= Instant::now() + Duration::from_secs(28800));
        drop(writer);
        fs::remove_dir_all(&config.directory).unwrap();
    }

    #[test]
    fn switching_from_calendar_to_interval_initializes_a_persisted_deadline() {
        let mut config = config();
        config.rotation = RotationPolicy::Daily;
        drop(writer(&config));
        config.rotation = RotationPolicy::default();
        let resumed = writer(&config);
        assert!(resumed.state.deadline.is_some());
        drop(resumed);
        fs::remove_dir_all(&config.directory).unwrap();
    }

    #[test]
    fn completed_rename_retries_state_publication_before_accepting_another_record() {
        let config = config();
        let mut writer = writer(&config);
        writer.write_all(b"before\n").unwrap();
        let target = writer.target().unwrap().0;
        let next = ActiveState::new(&config, SystemTime::now()).unwrap();
        writer.state.pending = Some(target.file_name().unwrap().to_str().unwrap().into());
        writer.state.next = Some(Box::new(next.clone()));
        save_json(&state_path(&config), &writer.state).unwrap();
        drop(writer.file.take());
        let active = Path::new(&config.directory).join("latest.log");
        fs::rename(&active, &target).unwrap();
        writer.file = Some(
            OpenOptions::new()
                .create_new(true)
                .append(true)
                .open(&active)
                .unwrap(),
        );
        writer.state = next;
        writer.bytes = 0;
        writer.state_dirty = true;
        let temporary = state_path(&config).with_file_name(".latest.log.active.json.tmp");
        fs::create_dir(&temporary).unwrap();
        assert!(writer.write_all(b"next\n").is_err());
        assert_eq!(fs::metadata(&active).unwrap().len(), 0);
        assert!(pending_fragment(&config).unwrap().is_some());
        fs::remove_dir(temporary).unwrap();
        writer.write_all(b"next\n").unwrap();
        assert!(pending_fragment(&config).unwrap().is_none());
        assert_eq!(fs::read(active).unwrap(), b"next\n");
        assert_eq!(fs::read(target).unwrap(), b"before\n");
        drop(writer);
        fs::remove_dir_all(&config.directory).unwrap();
    }

    #[test]
    fn failed_record_truncation_is_retried_before_size_rotation() {
        let config = config();
        let mut writer = writer(&config);
        writer.write_all(b"1234567\n").unwrap();
        writer.file.as_mut().unwrap().write_all(b"partial").unwrap();
        writer.bytes += 7;
        writer.rollback = Some(8);
        writer.write_all(b"next\n").unwrap();
        let fragment = fs::read_dir(&config.directory)
            .unwrap()
            .flatten()
            .find(|entry| entry.file_name().to_string_lossy().contains(".segment."))
            .unwrap();
        assert_eq!(fs::read(fragment.path()).unwrap(), b"1234567\n");
        assert_eq!(writer.bytes, 5);
        drop(writer);
        fs::remove_dir_all(&config.directory).unwrap();
    }

    #[test]
    fn interval_fragment_keeps_its_starting_local_date_across_midnight() {
        use chrono::TimeZone;
        let config = config();
        let start: SystemTime = Local
            .with_ymd_and_hms(2020, 1, 1, 23, 59, 0)
            .earliest()
            .unwrap()
            .into();
        let mut writer = RotatingFileWriter::new(
            &config,
            Arc::new(Mutex::new(None)),
            Arc::new(AtomicUsize::new(0)),
            start,
        )
        .unwrap();
        writer.write_at(b"one\n", start).unwrap();
        writer
            .write_at(b"two\n", start + Duration::from_secs(120))
            .unwrap();
        assert_eq!(writer.state.date, "2020-01-01");
        assert_eq!(writer.bytes, 8);
        assert_eq!(writer.state.sequence, 1);
        drop(writer);
        fs::remove_dir_all(&config.directory).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_rename_failure_keeps_the_active_file_and_retries_after_unlock() {
        use std::os::windows::fs::OpenOptionsExt;
        let config = config();
        let mut writer = writer(&config);
        writer.write_all(b"1234567\n").unwrap();
        let path = Path::new(&config.directory).join("latest.log");
        let handle = OpenOptions::new()
            .read(true)
            .share_mode(0x00000003)
            .open(&path)
            .unwrap();
        assert!(writer.write_all(b"next\n").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"1234567\n");
        drop(handle);
        writer.write_all(b"next\n").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"next\n");
        drop(writer);
        fs::remove_dir_all(&config.directory).unwrap();
    }
}
