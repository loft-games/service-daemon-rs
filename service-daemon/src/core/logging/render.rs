use std::fmt::Write as _;
use std::io::{Write as _, stderr};

use super::model::LogEvent;

// clippy 1.95 false-positive: `missing_const_for_thread_local` still fires
// despite the initializer already being wrapped in a `const {}` block.
#[allow(clippy::missing_const_for_thread_local)]
mod log_processing_flag {
    use std::cell::Cell;

    thread_local! {
        /// Thread-local flag set to `true` while `log_service` is processing a log event.
        /// Checked by `DaemonLayer::on_event()` to prevent recursive queue insertion.
        pub(super) static IN_LOG_PROCESSING: Cell<bool> = const { Cell::new(false) };
    }
}

use log_processing_flag::IN_LOG_PROCESSING;

/// RAII guard that marks the current thread as "inside log processing".
/// On drop (including panic unwinding), the flag is automatically cleared.
pub(super) struct LogProcessingGuard;

impl LogProcessingGuard {
    /// Activates the reentrancy guard for the current thread.
    pub(super) fn enter() -> Self {
        IN_LOG_PROCESSING.with(|f| f.set(true));
        LogProcessingGuard
    }
}

impl Drop for LogProcessingGuard {
    fn drop(&mut self) {
        IN_LOG_PROCESSING.with(|f| f.set(false));
    }
}

pub(super) fn in_log_processing() -> bool {
    IN_LOG_PROCESSING.with(|f| f.get())
}

/// Renders a log event into the provided buffer with ANSI color codes.
///
/// The buffer is cleared but NOT deallocated, allowing memory reuse across
/// successive calls within a batch loop.
pub(super) fn render_to_buf(event: &LogEvent, buf: &mut String) {
    buf.clear();
    let (color, reset) = event.level.ansi_color();
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
    if let Some(sid) = event.service_id {
        let _ = write!(buf, " service_id={}", sid);
    }
    if let Some(src_sid) = event.source_service_id {
        let _ = write!(buf, " source_service_id={}", src_sid);
    }
    if let Some(mid) = event.message_id {
        let _ = write!(buf, " message_id={}", mid);
    }
    if let Some(ref iid) = event.instance_id {
        let _ = write!(buf, " instance_id={}", iid);
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
pub(super) fn render_to_string(event: &LogEvent) -> String {
    let mut buf = String::with_capacity(256);
    render_to_buf(event, &mut buf);
    buf
}

/// Renders a log event to stderr using ANSI color coding and structured fields.
///
/// Thin wrapper around `render_to_buf` that performs a single atomic write
/// to stderr to avoid interleaved output from concurrent threads.
pub(super) fn render_to_stderr(event: &LogEvent) {
    let mut buf = String::with_capacity(256);
    render_to_buf(event, &mut buf);
    buf.push('\n');

    let stderr = stderr();
    let _ = stderr.lock().write_all(buf.as_bytes());
}

/// Formats a `LogEvent` as a structured JSON string for file persistence.
///
/// Output includes `level`, `time` (ISO 8601), `target`,
/// `msg`, `caller` (file:line), and `module_path`.
#[cfg(feature = "file-logging")]
pub(super) fn format_event_json(event: &LogEvent) -> String {
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
