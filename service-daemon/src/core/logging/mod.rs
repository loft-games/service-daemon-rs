mod layer;
pub(crate) mod model;
mod render;
mod services;

#[cfg(feature = "file-logging")]
mod file;

pub use layer::DaemonLayer;
pub use model::{LogBatchSizeError, MAX_LOG_BATCH_SIZE, set_log_batch_size};

#[cfg(feature = "file-logging")]
pub use file::{FileLogConfig, RotationPolicy, enable_file_logging};

use tracing_subscriber::prelude::*;

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
/// use service_daemon::DaemonLayer;
/// use tracing_subscriber::prelude::*;
///
/// tracing_subscriber::registry()
///     .with(tracing_subscriber::EnvFilter::new("info"))
///     .with(DaemonLayer)
///     .with(my_sentry_layer)
///     .init();
/// ```
///
/// File logging is configured separately via `enable_file_logging()` and
/// consumed by the independent `file_log_service`.
///
/// If a global tracing subscriber has already been set, this function logs a
/// warning through the existing subscriber and leaves it unchanged. Use
/// [`try_init_logging()`] when callers need to observe that condition.
pub fn init_logging() {
    if let Err(err) = try_init_logging() {
        tracing::warn!(
            error = %err,
            "init_logging skipped because a global tracing subscriber is already initialized"
        );
    }
}

/// Fallible variant of [`init_logging()`] for callers that need explicit
/// initialization status.
///
/// Returns `Err` instead of logging a warning when a global subscriber has
/// already been set. This is safe to call from multiple `#[tokio::test]`
/// functions running in parallel.
///
/// # Example
/// ```rust,ignore
/// #[tokio::test]
/// async fn my_test() {
///     let _ = service_daemon::try_init_logging();
///     // ... test logic
/// }
/// ```
pub fn try_init_logging() -> Result<(), tracing_subscriber::util::TryInitError> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(DaemonLayer)
        .try_init()
}

#[cfg(test)]
mod tests {
    use super::model::{
        DEFAULT_BATCH_SIZE, LOG_QUEUE_BATCH_MULTIPLIER, LogBatchSizeError, LogEvent, LogLevel,
        MAX_LOG_BATCH_SIZE, MAX_LOG_QUEUE_CAPACITY, get_log_queue,
        log_queue_capacity_for_batch_size, set_log_batch_size_in, validate_log_batch_size,
    };
    use super::render::render_to_string;
    use super::*;
    use crate::models::{ServiceId, service::InstanceId};
    use chrono::Utc;
    use std::borrow::Cow;
    use std::num::NonZeroUsize;
    use std::sync::{Arc, OnceLock};
    use uuid::Uuid;

    #[test]
    fn init_logging_does_not_panic_when_global_subscriber_already_exists() {
        let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry());

        let result = std::panic::catch_unwind(init_logging);

        assert!(result.is_ok());
    }

    #[test]
    fn log_batch_size_accepts_documented_maximum() {
        assert_eq!(validate_log_batch_size(MAX_LOG_BATCH_SIZE), Ok(()));
        assert_eq!(
            log_queue_capacity_for_batch_size(MAX_LOG_BATCH_SIZE),
            Ok(MAX_LOG_QUEUE_CAPACITY)
        );
    }

    #[test]
    fn log_batch_size_rejects_values_above_documented_maximum() {
        let requested = MAX_LOG_BATCH_SIZE + 1;
        assert_eq!(
            validate_log_batch_size(requested),
            Err(LogBatchSizeError::TooLarge {
                requested,
                max: MAX_LOG_BATCH_SIZE,
            })
        );
        assert_eq!(
            log_queue_capacity_for_batch_size(requested),
            Err(LogBatchSizeError::TooLarge {
                requested,
                max: MAX_LOG_BATCH_SIZE,
            })
        );
    }

    #[test]
    fn log_queue_capacity_is_derived_from_valid_batch_size() {
        assert_eq!(
            log_queue_capacity_for_batch_size(DEFAULT_BATCH_SIZE),
            Ok(DEFAULT_BATCH_SIZE * LOG_QUEUE_BATCH_MULTIPLIER)
        );
    }

    #[test]
    fn set_log_batch_size_in_rejects_duplicate_configuration() {
        let storage = OnceLock::new();
        let first = NonZeroUsize::new(256).unwrap();
        let second = NonZeroUsize::new(512).unwrap();

        assert_eq!(set_log_batch_size_in(&storage, false, first), Ok(()));
        assert_eq!(
            set_log_batch_size_in(&storage, false, second),
            Err(LogBatchSizeError::AlreadyInitialized {
                requested: 512,
                active: 256,
            })
        );
    }

    #[test]
    fn set_log_batch_size_in_rejects_late_configuration() {
        let storage = OnceLock::new();
        let requested = NonZeroUsize::new(512).unwrap();

        assert_eq!(
            set_log_batch_size_in(&storage, true, requested),
            Err(LogBatchSizeError::AlreadyInitialized {
                requested: 512,
                active: DEFAULT_BATCH_SIZE,
            })
        );
    }

    // -----------------------------------------------------------------------
    // Helper: build a synthetic LogEvent with specified fields for testing
    // -----------------------------------------------------------------------
    fn make_event(
        level: LogLevel,
        message: &str,
        service_id: Option<ServiceId>,
        source_service_id: Option<ServiceId>,
        message_id: Option<Uuid>,
        instance_id: Option<InstanceId>,
        error_chain: Option<&str>,
    ) -> LogEvent {
        LogEvent {
            timestamp: Utc::now(),
            level,
            target: Cow::Borrowed("test::target"),
            message: message.to_string(),
            module_path: None,
            file: None,
            line: None,
            service_id,
            source_service_id,
            message_id,
            instance_id,
            error_chain: error_chain.map(|s| s.to_string()),
        }
    }

    // =======================================================================
    // 5A: ConsoleRenderer ANSI color output tests
    // =======================================================================

    #[test]
    fn render_info_level_contains_green_ansi_code() {
        let event = make_event(LogLevel::Info, "hello world", None, None, None, None, None);
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
        let event = make_event(
            LogLevel::Error,
            "something broke",
            None,
            None,
            None,
            None,
            None,
        );
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
        let event = make_event(LogLevel::Warn, "caution", None, None, None, None, None);
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
        let event = make_event(
            LogLevel::Debug,
            "verbose detail",
            None,
            None,
            None,
            None,
            None,
        );
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
        let event = make_event(
            LogLevel::Info,
            "msg",
            Some(ServiceId::new(123)),
            None,
            None,
            None,
            None,
        );
        let output = render_to_string(&event);
        assert!(
            output.contains("service_id=svc#123"),
            "output should contain service_id, got: {}",
            output
        );
    }

    #[test]
    fn render_includes_all_ids_when_present() {
        let test_iid = InstanceId::new(ServiceId::new(3), 0);
        let msg_id = Uuid::parse_str("0195e342-8874-7065-a86d-3e6a457b0195").unwrap();
        let event = make_event(
            LogLevel::Info,
            "triggered",
            Some(ServiceId::new(1)),
            None,
            Some(msg_id),
            Some(test_iid),
            None,
        );
        let output = render_to_string(&event);
        assert!(output.contains("service_id=svc#1"), "missing service_id");
        assert!(
            output.contains("message_id=0195e342-8874-7065-a86d-3e6a457b0195"),
            "missing message_id"
        );
        assert!(
            output.contains("instance_id=svc#3:0"),
            "missing instance_id, got: {}",
            output
        );
    }

    #[test]
    fn render_includes_error_chain_when_present() {
        let event = make_event(
            LogLevel::Error,
            "operation failed",
            None,
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
        let event = make_event(LogLevel::Info, "init phase", None, None, None, None, None);
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
            !output.contains("instance_id="),
            "should not contain instance_id when None"
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
    /// Uses `tracing::subscriber::with_default` for test isolation - does NOT
    /// set a global subscriber, so tests can run in parallel.
    fn collect_events_with_daemon_layer(f: impl FnOnce()) -> Vec<Arc<LogEvent>> {
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
            let span = tracing::info_span!("service", service_id = "svc#42",);
            let _enter = span.enter();
            tracing::info!("svc_id_test_marker");
        });

        let event = events
            .iter()
            .find(|e| e.message.contains("svc_id_test_marker"))
            .expect("should have captured the event");

        assert_eq!(
            event.service_id,
            Some(ServiceId::new(42)),
            "service_id should be extracted from Span"
        );
    }

    #[test]
    fn daemon_layer_captures_message_id_from_nested_span() {
        let msg_id_str = "0195e342-8874-7065-a86d-3e6a457b0195";
        let msg_id = Uuid::parse_str(msg_id_str).unwrap();

        let events = collect_events_with_daemon_layer(|| {
            let service_span = tracing::info_span!("service", service_id = "svc#1",);
            let _svc_enter = service_span.enter();

            let trigger_span = tracing::info_span!(
                "trigger",
                service_id = "svc#2",
                message_id = msg_id_str,
                instance_svc_id = 3u64,
                instance_seq = 7u64,
            );
            let _trig_enter = trigger_span.enter();
            tracing::info!("nested_span_test_marker");
        });

        let event = events
            .iter()
            .find(|e| e.message.contains("nested_span_test_marker"))
            .expect("should have captured the event");

        assert_eq!(
            event.service_id,
            Some(ServiceId::new(2)),
            "service_id should come from innermost span"
        );
        assert_eq!(
            event.message_id,
            Some(msg_id),
            "message_id should be extracted from trigger span"
        );
        let expected_iid = InstanceId::new(ServiceId::new(3), 7);
        assert_eq!(
            event.instance_id,
            Some(expected_iid),
            "instance_id should be reconstructed from numeric fields"
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
            event.instance_id.is_none(),
            "instance_id should be None outside span"
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
