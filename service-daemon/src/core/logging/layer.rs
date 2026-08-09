use std::borrow::Cow;
use std::str::FromStr;
use std::sync::Arc;

use chrono::Utc;
use tracing::{Event, Subscriber, field};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;
use uuid::Uuid;

use crate::models::{ServiceInstanceId, service::TriggerInstanceId};

use super::model::{LogEvent, LogLevel, get_log_queue};
use super::render::{in_log_processing, render_to_stderr};

/// Collects the message and structured fields from a tracing event.
///
/// Implements dual-path capture:
/// - `record_str`: called for `&str` values, produces clean output without Debug quotes.
/// - `record_debug`: fallback for `fmt::Arguments`, `u64`, `bool`, etc.
///   `fmt::Arguments::Debug` delegates to `Display` (no extra quotes).
struct FieldCollector {
    message: String,
    fields: Vec<(String, String)>,
    /// Captured error chain from `record_error` or an `error` named field.
    error_chain: Option<String>,
}

impl FieldCollector {
    fn new() -> Self {
        Self {
            message: String::new(),
            fields: Vec::new(),
            error_chain: None,
        }
    }

    /// Builds the final message string.
    /// If structured fields are present, appends them as `{ key=value, ... }`.
    fn build_message(&self) -> String {
        if self.fields.is_empty() {
            self.message.clone()
        } else {
            let pairs: Vec<String> = self
                .fields
                .iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect();
            format!("{} {{ {} }}", self.message, pairs.join(", "))
        }
    }

    /// Extracts the captured error chain, if any.
    fn take_error(&mut self) -> Option<String> {
        self.error_chain.take()
    }
}

impl tracing::field::Visit for FieldCollector {
    /// Priority path for `&str` values. Avoids Debug quote wrapping on the message field.
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }
    }

    /// Records non-string types (`fmt::Arguments`, `u64`, `bool`, etc.).
    /// `fmt::Arguments::Debug` delegates to `Display`, so no extra quotes are added
    /// for formatted messages like `tracing::info!("port = {}", 80)`.
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let formatted = format!("{:?}", value);
        if field.name() == "message" {
            self.message = formatted;
        } else if field.name() == "error" {
            // Capture error fields separately for structured logging
            self.error_chain = Some(formatted);
        } else {
            self.fields.push((field.name().to_string(), formatted));
        }
    }
}

/// A non-blocking tracing Layer that captures events and pushes them to the LogQueue.
///
/// When reentrancy is detected (i.e., `log_service` emits a tracing event while
/// processing a log), the event bypasses the queue and is written directly to stderr
/// to prevent infinite recursion.
///
/// # Span Context Extraction
///
/// `DaemonLayer` requires `LookupSpan` on the subscriber so it can walk the
/// current Span chain and extract `service_instance_id`, `message_id`, and `trigger_instance_id`
/// fields that were injected by `ServiceSupervisor::on_running` and
/// `TracingInterceptor`. Events outside any Span will have `None` for these IDs.
pub struct DaemonLayer;

impl<S> Layer<S> for DaemonLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    /// Captures Span field values (`service_instance_id`, `message_id`, etc.) into the
    /// Span's extensions on creation. These are later read by `extract_span_ids`
    /// during event processing.
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        if let Some(span) = ctx.span(id) {
            let mut visitor = SpanFieldVisitor::default();
            attrs.record(&mut visitor);

            // Only store if at least one known field was found
            if visitor.fields.service_instance_id.is_some()
                || visitor.fields.message_id.is_some()
                || visitor.fields.source_service_instance_id.is_some()
                || visitor.fields.instance_seq.is_some()
            {
                span.extensions_mut().insert(visitor.fields);
            }
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let metadata = event.metadata();

        // Collect message and structured fields from the event
        let mut collector = FieldCollector::new();
        event.record(&mut collector);
        let message = collector.build_message();
        let error_chain = collector.take_error();

        // Walk the Span chain to extract service/trigger IDs
        let (service_instance_id, source_service_instance_id, message_id, trigger_instance_id) =
            extract_span_ids(&ctx, event);

        let log_event = Arc::new(LogEvent {
            timestamp: Utc::now(),
            level: LogLevel::from_tracing(metadata.level()),
            target: Cow::Borrowed(metadata.target()),
            message,
            module_path: metadata.module_path().map(Cow::Borrowed),
            file: metadata.file().map(Cow::Borrowed),
            line: metadata.line(),
            service_instance_id,
            source_service_instance_id,
            message_id,
            trigger_instance_id,
            error_chain,
        });

        // Reentrancy check: if log_service is currently processing a log event
        // on this thread, bypass the queue and write directly to stderr.
        if in_log_processing() {
            render_to_stderr(&log_event);
            return;
        }

        // Normal path: non-blocking send to the broadcast queue
        let _ = get_log_queue().tx.send(log_event);
    }
}

/// Walks the current Span scope to extract `service_instance_id`, `message_id`, and
/// `trigger_instance_id` fields from ancestor Spans.
///
/// These fields are injected by:
/// - `ServiceSupervisor::on_running` - creates `info_span!("service", service_instance_id = ...)`
/// - `TracingInterceptor` - creates `info_span!("trigger", service_instance_id = ..., message_id = ...)`
///
/// The walk proceeds from innermost (current) to outermost Span, returning
/// the first value found for each field.
fn extract_span_ids<S>(
    ctx: &Context<'_, S>,
    event: &Event<'_>,
) -> (
    Option<ServiceInstanceId>,
    Option<ServiceInstanceId>,
    Option<Uuid>,
    Option<TriggerInstanceId>,
)
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    let mut service_instance_id = None;
    let mut source_service_instance_id = None;
    let mut message_id = None;
    let mut trigger_instance_id = None;

    if let Some(scope) = ctx.event_scope(event) {
        for span in scope {
            let extensions = span.extensions();

            // 1. Native typed IDs stored in span extensions.
            if service_instance_id.is_none()
                && let Some(sid) = extensions.get::<ServiceInstanceId>()
            {
                service_instance_id = Some(*sid);
            }
            if message_id.is_none()
                && let Some(mid) = extensions.get::<Uuid>()
            {
                message_id = Some(*mid);
            }
            if trigger_instance_id.is_none()
                && let Some(iid) = extensions.get::<TriggerInstanceId>()
            {
                trigger_instance_id = Some(*iid);
            }

            // 2. String span fields from tracing spans.
            if let Some(fields) = extensions.get::<SpanFields>() {
                if service_instance_id.is_none()
                    && let Some(ref s) = fields.service_instance_id
                    && let Ok(id) = ServiceInstanceId::from_str(s)
                {
                    service_instance_id = Some(id);
                }
                if source_service_instance_id.is_none()
                    && let Some(ref s) = fields.source_service_instance_id
                    && let Ok(id) = ServiceInstanceId::from_str(s)
                {
                    source_service_instance_id = Some(id);
                }
                if message_id.is_none()
                    && let Some(ref s) = fields.message_id
                    && let Ok(id) = Uuid::parse_str(s)
                {
                    message_id = Some(id);
                }

                // Trigger instance reconstruction from span fields.
                if trigger_instance_id.is_none()
                    && let (Some(svc), Some(seq)) = (service_instance_id, fields.instance_seq)
                {
                    trigger_instance_id = Some(TriggerInstanceId::new(svc, seq));
                }
            }
        }
    }

    (
        service_instance_id,
        source_service_instance_id,
        message_id,
        trigger_instance_id,
    )
}

/// Storage for extracted span field values, attached to each Span via extensions.
///
/// When `DaemonLayer` sees a new Span with known field names (`service_instance_id`,
/// `message_id`, etc.), it stores their values in a `SpanFields` instance
/// within the Span's extensions. These values are later read by
/// `extract_span_ids` during event processing.
#[derive(Debug, Default)]
struct SpanFields {
    service_instance_id: Option<String>,
    message_id: Option<String>,
    /// The `ServiceInstanceId` of the source service.
    source_service_instance_id: Option<String>,
    /// High 64 bits of `message_id` (Uuid).
    mid_hi: Option<u64>,
    /// Low 64 bits of `message_id` (Uuid).
    mid_lo: Option<u64>,
    /// Numeric sequence component of the trigger instance identifier.
    instance_seq: Option<u64>,
}

/// Visitor that extracts known ID fields from Span attributes during creation.
///
/// Recognizes:
/// - `service_instance_id` - from `ServiceSupervisor::on_running` and `TracingInterceptor`
/// - `message_id` - from `TracingInterceptor` (trigger dispatch)
/// - `source_service_instance_id` / `instance_seq` - trigger context fields from `TracingInterceptor`
///
/// All other fields are ignored. String values are captured via `Display`
/// formatting; numeric values are captured via `record_u64`.
#[derive(Debug, Default)]
struct SpanFieldVisitor {
    fields: SpanFields,
}

impl field::Visit for SpanFieldVisitor {
    fn record_debug(&mut self, field: &field::Field, value: &dyn std::fmt::Debug) {
        let formatted = format!("{:?}", value);
        match field.name() {
            "service_instance_id" => self.fields.service_instance_id = Some(formatted),
            "source_service_instance_id" => {
                self.fields.source_service_instance_id = Some(formatted)
            }
            "message_id" => self.fields.message_id = Some(formatted),
            _ => {} // Ignore unknown fields
        }
    }

    fn record_str(&mut self, field: &field::Field, value: &str) {
        match field.name() {
            "service_instance_id" => self.fields.service_instance_id = Some(value.to_string()),
            "source_service_instance_id" => {
                self.fields.source_service_instance_id = Some(value.to_string())
            }
            "message_id" => self.fields.message_id = Some(value.to_string()),
            _ => {}
        }
    }

    fn record_u64(&mut self, field: &field::Field, value: u64) {
        match field.name() {
            "mid_hi" => self.fields.mid_hi = Some(value),
            "mid_lo" => self.fields.mid_lo = Some(value),
            "instance_seq" => self.fields.instance_seq = Some(value),
            _ => {}
        }
    }
}
