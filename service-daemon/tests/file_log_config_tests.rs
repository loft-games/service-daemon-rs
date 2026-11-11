#![cfg(feature = "file-logging")]
use service_daemon::{FileLogConfig, RotationPolicy};
use std::time::Duration;

#[test]
fn defaults_pair_rotation_with_three_compression_stages() {
    let config = FileLogConfig::default();
    assert!(
        matches!(config.rotation, RotationPolicy::Interval(value) if value == Duration::from_secs(8 * 3600))
    );
    assert_eq!(config.max_file_size, Some(100 * 1024 * 1024));
    assert_eq!(config.rotation_jitter, 0.05);
    assert_eq!(config.archive.fragment_compression_level, 1);
    assert_eq!(config.archive.daily_compression_level, 6);
    assert_eq!(config.archive.monthly_compression_level, 9);
    assert_eq!(config.archive.monthly_volume_size, 1024 * 1024 * 1024);
    assert_eq!(config.archive.daily_start_jitter, Duration::from_secs(1800));
    assert_eq!(
        config.archive.monthly_start_jitter,
        Duration::from_secs(1800)
    );
    assert_eq!(
        config.archive.monthly_volume_jitter,
        Duration::from_secs(1800)
    );
    assert_eq!(
        config.archive.daily_archive_delay,
        Duration::from_secs(86400)
    );
    config.validate().unwrap();
}

#[test]
fn archive_jitter_accepts_zero_and_rejects_unrepresentable_delays() {
    for stage in 0..3 {
        let mut config = FileLogConfig::default();
        let delay = match stage {
            0 => &mut config.archive.daily_start_jitter,
            1 => &mut config.archive.monthly_start_jitter,
            _ => &mut config.archive.monthly_volume_jitter,
        };
        *delay = Duration::MAX;
        assert!(config.validate().is_err());
    }
    let mut config = FileLogConfig::default();
    config.archive.daily_start_jitter = Duration::ZERO;
    config.archive.monthly_start_jitter = Duration::ZERO;
    config.archive.monthly_volume_jitter = Duration::ZERO;
    config.validate().unwrap();
}

#[test]
fn invalid_configuration_is_rejected_before_opening_files() {
    let mut config = FileLogConfig {
        max_file_size: Some(0),
        ..FileLogConfig::default()
    };
    assert!(config.validate().is_err());
    config.max_file_size = None;
    for jitter in [f64::NAN, f64::INFINITY, -0.01, 1.0] {
        config.rotation_jitter = jitter;
        assert!(config.validate().is_err());
    }
    config.rotation_jitter = 0.0;
    config.rotation = RotationPolicy::Interval(Duration::ZERO);
    assert!(config.validate().is_err());
    config.rotation = RotationPolicy::Never;
    config.archive.monthly_volume_size = 0;
    assert!(config.validate().is_err());
    config.archive.monthly_volume_size = 1;
    for level in [10, u32::MAX] {
        config.archive.daily_compression_level = level;
        assert!(config.validate().is_err());
    }
}

#[test]
fn invalid_setup_does_not_consume_the_process_global_configuration_slot() {
    use service_daemon::{FileLogConfigError, try_enable_file_logging};
    let config = FileLogConfig {
        max_file_size: Some(0),
        ..FileLogConfig::default()
    };
    assert!(matches!(
        try_enable_file_logging(config),
        Err(FileLogConfigError::Invalid(_))
    ));
    try_enable_file_logging(FileLogConfig::default()).unwrap();
    assert!(matches!(
        try_enable_file_logging(FileLogConfig::default()),
        Err(FileLogConfigError::AlreadyEnabled)
    ));
}
