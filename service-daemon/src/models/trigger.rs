/// Template types for event-driven triggers.
///
/// This enum provides a structured way to specify trigger hosts in the `#[trigger]` macro,
/// replacing loose strings with typed variants for better IDE support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerTemplate {
    // --- Signal-based (uses signal_trigger_host) ---
    /// Generic notification/event signal.
    Notify,
    /// Alias for Notify.
    Event,
    /// Alias for Notify.
    Signal,
    /// Custom signal-based trigger.
    Custom,

    // --- Fanout Queues (uses queue_trigger_host) ---
    /// Broadcast queue where all handlers receive every message.
    Queue,
    /// Alias for Queue (Broadcast Queue).
    BQueue,
    /// Alias for Queue.
    BroadcastQueue,

    // --- Load-Balancing Queues (uses lb_queue_trigger_host) ---
    /// Load-balanced queue where messages are distributed to a single handler.
    LBQueue,
    /// Alias for LBQueue.
    LoadBalancingQueue,

    // --- Scheduled (uses cron_trigger_host) ---
    /// Cron-based scheduled trigger.
    Cron,

    // --- State-driven (uses watch_trigger_host) ---
    /// Fires when a specific state provider is modified.
    Watch,
    /// Alias for Watch.
    State,
}

/// Short alias for TriggerTemplate.
pub use TriggerTemplate as TT;

// ---------------------------------------------------------------------------
// TriggerMessage — traceable event payload (the "stone" in the ripple model)
// ---------------------------------------------------------------------------

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::service::ServiceId;
use crate::core::di::Provided;

/// A traceable event message that flows through the trigger system.
///
/// Every signal emitted by a service is wrapped in a `TriggerMessage` so that
/// downstream triggers can inspect the origin (`source_id`) and correlate
/// related events (`message_id`).
///
/// # Identity Model
/// - `message_id`: A globally unique identifier for this specific event instance.
/// - `source_id`: The `ServiceId` of the service that published this message.
///
/// These two fields together enable full-chain traceability: given any trigger
/// invocation you can answer "which stone caused this ripple?".
#[derive(Debug, Clone)]
pub struct TriggerMessage<P> {
    /// Globally unique identifier for this event instance.
    pub message_id: String,
    /// The `ServiceId` of the service that published this message.
    pub source_id: ServiceId,
    /// Timestamp when the message was created.
    pub timestamp: DateTime<Utc>,
    /// The business payload carried by this message.
    pub payload: P,
}

// ---------------------------------------------------------------------------
// TriggerContext — runtime context available to every trigger handler
// ---------------------------------------------------------------------------

/// Runtime context passed to a trigger handler when an event is captured.
///
/// Combines the trigger service's own identity with the incoming message,
/// providing everything the handler needs for processing and tracing.
///
/// # Instance ID
/// The `trigger_instance_id()` method produces a hierarchical identifier in
/// the format `svc#N:SEQ`, linking each handler invocation back to its
/// owning trigger service.
#[derive(Debug, Clone)]
pub struct TriggerContext<P> {
    /// The `ServiceId` of the trigger service that captured this event.
    pub service_id: ServiceId,
    /// Monotonically increasing sequence number within this trigger service.
    pub instance_seq: u64,
    /// The incoming message that triggered this invocation.
    pub message: TriggerMessage<P>,
}

impl<P> TriggerContext<P> {
    /// Produces a hierarchical instance identifier (e.g. `svc#1:42`).
    ///
    /// This links the handler invocation to a specific trigger service and
    /// a specific sequence number within that service's lifetime.
    pub fn trigger_instance_id(&self) -> String {
        format!("{}:{}", self.service_id, self.instance_seq)
    }
}

// ---------------------------------------------------------------------------
// TriggerHandler — unified async handler signature
// ---------------------------------------------------------------------------

/// The canonical function signature for trigger event handlers.
///
/// Every trigger host invokes a handler of this shape, providing a
/// `TriggerContext` with full traceability information.
pub type TriggerHandler<P> = Arc<
    dyn Fn(TriggerContext<P>) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync + 'static,
>;

// ---------------------------------------------------------------------------
// TriggerHost — extensible trigger engine trait
// ---------------------------------------------------------------------------

/// Defines a pluggable trigger engine ("reed in the water").
///
/// Any type implementing `TriggerHost` can serve as a trigger template,
/// removing the need to modify framework source code when adding new
/// trigger types. The built-in hosts (`SignalTriggerHost`, `TopicHost`,
/// `CronHost`, etc.) will all implement this trait.
///
/// # How a trigger perceives events
///
/// The concrete implementation decides the underlying mechanism:
/// - **Notify-based**: The host awaits `tokio::sync::Notify::notified()`.
///   Ideal for lightweight, payload-free signals.
/// - **Queue-based**: The host awaits `tokio::sync::broadcast::Receiver::recv()`.
///   Ideal for payload-carrying events with fan-out semantics.
/// - **Custom**: Any async waiting mechanism (e.g. file-system watcher,
///   external message broker) can be wrapped in a `TriggerHost`.
///
/// # Example (user-defined host)
/// ```rust,ignore
/// pub struct WebhookHost;
///
/// impl TriggerHost for WebhookHost {
///     type Target = WebhookConfig;  // resolved via DI
///     type Payload = WebhookEvent;
///
///     fn run_as_service(
///         name: String,
///         target: Self::Target,
///         handler: TriggerHandler<Self::Payload>,
///         token: CancellationToken,
///     ) -> BoxFuture<'static, anyhow::Result<()>> {
///         Box::pin(async move { /* listen for webhooks … */ Ok(()) })
///     }
/// }
/// ```
pub trait TriggerHost {
    /// The configuration / event-source type, resolved via the DI system.
    type Target: Provided;
    /// The business payload type carried by each event.
    type Payload: Send + Sync + 'static;

    /// Start the trigger's event loop as a long-running service.
    ///
    /// The implementation should:
    /// 1. Resolve / subscribe to the underlying event source via `target`.
    /// 2. Loop, awaiting events until `token` is cancelled.
    /// 3. For each event, construct a `TriggerContext` and invoke `handler`.
    fn run_as_service(
        name: String,
        target: Self::Target,
        handler: TriggerHandler<Self::Payload>,
        token: CancellationToken,
    ) -> BoxFuture<'static, anyhow::Result<()>>;
}
