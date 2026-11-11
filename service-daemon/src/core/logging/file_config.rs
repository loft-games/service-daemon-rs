//! Public file logging policy, independent of worker state.
use std::time::{Duration, Instant, SystemTime};

/// Time condition for rotation. Size is a separate condition, except for `Never`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationPolicy {
    /// Rotate on a local calendar day boundary.
    Daily,
    /// Rotate on a local calendar hour boundary.
    Hourly,
    /// Rotate after an elapsed interval, independently jittered for each file.
    Interval(Duration),
    /// Disable rotation and archive maintenance, including size rotation.
    Never,
}
impl Default for RotationPolicy {
    fn default() -> Self {
        Self::Interval(Duration::from_secs(8 * 3600))
    }
}

/// Compression switch. Levels for each stage live in [`FileLogArchiveConfig`].
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum FileLogCompression {
    /// Leave closed fragments uncompressed; disable daily and monthly archival.
    None,
    /// Enable fragment, daily, and monthly gzip processing.
    #[default]
    Gzip,
}

/// Settings for the three archive stages. Sizes are bytes; gzip levels are 0–9.
#[derive(Debug, Clone)]
pub struct FileLogArchiveConfig {
    pub fragment_compression_level: u32,
    pub daily_compression_level: u32,
    pub monthly_compression_level: u32,
    /// Soft limit on compressed bytes per independent monthly gzip volume.
    pub monthly_volume_size: u64,
    /// Delay measured from the end of the fragment's starting local date.
    pub daily_archive_delay: Duration,
    /// Maximum random delay before an eligible daily archive starts; zero disables it.
    pub daily_start_jitter: Duration,
    /// Maximum random delay before an eligible monthly archive starts.
    pub monthly_start_jitter: Duration,
    /// Maximum random delay between independently published monthly volumes.
    pub monthly_volume_jitter: Duration,
}
impl Default for FileLogArchiveConfig {
    fn default() -> Self {
        Self {
            fragment_compression_level: 1,
            daily_compression_level: 6,
            monthly_compression_level: 9,
            monthly_volume_size: 1024 * 1024 * 1024,
            daily_archive_delay: Duration::from_secs(86400),
            daily_start_jitter: Duration::from_secs(30 * 60),
            monthly_start_jitter: Duration::from_secs(30 * 60),
            monthly_volume_jitter: Duration::from_secs(30 * 60),
        }
    }
}

/// File logging configuration. An active filename is preserved exactly.
#[derive(Debug, Clone)]
pub struct FileLogConfig {
    pub directory: String,
    pub file_prefix: String,
    pub rotation: RotationPolicy,
    pub compression: FileLogCompression,
    /// Uncompressed fragment size; `None` disables the size condition.
    pub max_file_size: Option<u64>,
    /// Symmetric fractional jitter in [0, 1), used only by `Interval`.
    pub rotation_jitter: f64,
    pub archive: FileLogArchiveConfig,
}
impl Default for FileLogConfig {
    fn default() -> Self {
        Self {
            directory: "logs".into(),
            file_prefix: "latest.log".into(),
            rotation: RotationPolicy::default(),
            compression: FileLogCompression::default(),
            max_file_size: Some(100 * 1024 * 1024),
            rotation_jitter: 0.05,
            archive: FileLogArchiveConfig::default(),
        }
    }
}
impl FileLogConfig {
    #[must_use]
    pub fn new(directory: impl Into<String>, file_prefix: impl Into<String>) -> Self {
        Self {
            directory: directory.into(),
            file_prefix: file_prefix.into(),
            ..Self::default()
        }
    }
    /// Checks policy before any worker or file is created.
    pub fn validate(&self) -> Result<(), FileLogConfigError> {
        let invalid = |message| Err(FileLogConfigError::Invalid(message));
        if self.max_file_size == Some(0) {
            return invalid("max_file_size must be positive or None");
        }
        if self.archive.monthly_volume_size == 0 {
            return invalid("monthly_volume_size must be positive");
        }
        for (delay, message) in [
            (
                self.archive.daily_start_jitter,
                "daily_start_jitter exceeds the system clock range",
            ),
            (
                self.archive.monthly_start_jitter,
                "monthly_start_jitter exceeds the system clock range",
            ),
            (
                self.archive.monthly_volume_jitter,
                "monthly_volume_jitter exceeds the system clock range",
            ),
        ] {
            if SystemTime::now().checked_add(delay).is_none()
                || Instant::now().checked_add(delay).is_none()
            {
                return invalid(message);
            }
        }
        if !self.rotation_jitter.is_finite() || !(0.0..1.0).contains(&self.rotation_jitter) {
            return invalid("rotation_jitter must be finite and in [0, 1)");
        }
        if let RotationPolicy::Interval(interval) = self.rotation {
            let maximum =
                Duration::try_from_secs_f64(interval.as_secs_f64() * (1.0 + self.rotation_jitter));
            if interval.is_zero()
                || maximum.as_ref().is_err()
                || maximum.ok().is_none_or(|maximum| {
                    SystemTime::now().checked_add(maximum).is_none()
                        || Instant::now().checked_add(maximum).is_none()
                })
            {
                return invalid(
                    "rotation interval must be positive and representable by the system clock",
                );
            }
        }
        for (level, message) in [
            (
                self.archive.fragment_compression_level,
                "fragment_compression_level must be in 0..=9",
            ),
            (
                self.archive.daily_compression_level,
                "daily_compression_level must be in 0..=9",
            ),
            (
                self.archive.monthly_compression_level,
                "monthly_compression_level must be in 0..=9",
            ),
        ] {
            if level > 9 {
                return invalid(message);
            }
        }
        if self.directory.is_empty()
            || self.file_prefix.is_empty()
            || self.file_prefix == "."
            || self.file_prefix == ".."
            || self.file_prefix.contains(['/', '\\', ':', '\0'])
        {
            return invalid(
                "directory and active filename must be nonempty; filename must be a single component",
            );
        }
        Ok(())
    }
}

/// Failure to configure the process-global file sink.
#[derive(Debug, thiserror::Error)]
pub enum FileLogConfigError {
    #[error("invalid file logging configuration: {0}")]
    Invalid(&'static str),
    #[error("file logging has already been configured")]
    AlreadyEnabled,
}
