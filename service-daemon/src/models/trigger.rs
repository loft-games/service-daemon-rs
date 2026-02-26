//! Trigger system models: `TriggerHost` trait, context types, and built-in aliases.
//!
//! The trigger system is built around the **Policy-Engine** separation:
//!
//! - **Policy** (`handle_step`): Defined by each trigger host. It only cares about
//!   *"how to wait for the next event"* and returns a [`TriggerTransition`].
//! - **Engine** (`run_as_service` default impl): Manages the event loop, tracing,
//!   instance ID issuance, and graceful shutdown. Host implementors get this for free.
//!
//! ## Extension Model
//!
//! To add a new trigger type, define a struct and implement `TriggerHost<T>`:
//!
//! ```rust,ignore
//! pub struct MyHost;
//!
//! impl<T: Provided + ...> TriggerHost<T> for MyHost {
//!     type Payload = MyEvent;
//!     fn handle_step(target: Arc<T>) -> BoxFuture<'static, TriggerTransition<Self::Payload>> {
//!         Box::pin(async move {
//!             let event = target.wait_for_event().await;
//!             TriggerTransition::Next(event)
//!         })
//!     }
//! }
//! ```
//!
//! No changes to the macro crate are needed — the `#[trigger]` macro resolves
//! everything through the type system at compile time.
//!
//! ## Built-in Aliases
//!
//! The [`TT`] module provides short aliases for the built-in trigger hosts so
//! users can write `#[trigger(Notify(MySignal))]` instead of
//! `#[trigger(SignalHost(MySignal))]`.

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, error, info};

use super::service::ServiceId;
use crate::core::context;

// ---------------------------------------------------------------------------
// TriggerMessage — traceable event payload (the "stone" in the ripple model)
// ---------------------------------------------------------------------------

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
// TriggerTransition — lifecycle handshake protocol
// ---------------------------------------------------------------------------

/// Lifecycle transition returned by [`TriggerHost::handle_step`].
///
/// This enum is the "handshake" protocol between the trigger policy (host)
/// and the trigger engine (default `run_as_service` loop). It tells the
/// engine what to do after one iteration:
///
/// - [`Next`](TriggerTransition::Next): Dispatch the payload and loop again.
/// - [`Reload`](TriggerTransition::Reload): Dispatch the payload, then idle
///   until the framework's `ServiceWatcher` restarts us (leveraging the
///   existing service reload mechanism).
/// - [`Stop`](TriggerTransition::Stop): Exit the event loop cleanly.
pub enum TriggerTransition<P> {
    /// Deliver the payload and continue the event loop.
    ///
    /// Used by streaming triggers (Signal, Queue, LBQueue) that process
    /// a continuous flow of events within a single service lifetime.
    Next(P),

    /// Deliver the payload, then idle until the framework restarts us.
    ///
    /// Used by state-watch triggers: fire once with the current snapshot,
    /// then wait. When the target provider changes, the `ServiceWatcher`
    /// will abort this instance and spawn a fresh one with updated state.
    Reload(P),

    /// Exit the event loop without dispatching. The trigger stops entirely.
    Stop,
}

// ---------------------------------------------------------------------------
// TriggerHost — the sole extension point for all trigger types
// ---------------------------------------------------------------------------

/// Defines a pluggable trigger engine, parameterized by target type `T`.
///
/// The generic parameter `T` is the event-source / configuration type that
/// the macro resolves via `<T as Provided>::resolve().await`. Making the
/// target a generic parameter (rather than an associated type) allows a
/// single host struct to work with **any** compatible target.
///
/// # Policy-Engine Separation
///
/// - **Policy**: Implement [`handle_step`](TriggerHost::handle_step) to define
///   your waiting logic. Return a [`TriggerTransition`] to tell the engine
///   what to do next.
/// - **Engine**: The default [`run_as_service`](TriggerHost::run_as_service)
///   implementation manages the event loop, tracing spans, instance IDs, and
///   graceful shutdown. You get all of this **for free**.
///
/// Override `run_as_service` only for hosts that cannot fit the
/// `handle_step` model (e.g., `CronHost` which uses external callbacks).
///
/// # Built-in implementations
///
/// | Host struct     | Underlying mechanism    | Payload |
/// |-----------------|-------------------------|---------|
/// | `SignalHost`    | `tokio::sync::Notify`   | `()`    |
/// | `TopicHost`     | `broadcast::Receiver`   | `T`     |
/// | `LBTopicHost`   | `mpsc::Receiver` (Mutex)| `T`     |
/// | `CronHost`      | `tokio-cron-scheduler`  | `()`    |
/// | `WatchHost`     | State change detection  | `()`    |
///
/// # User-defined hosts
///
/// ```rust,ignore
/// pub struct WebhookHost;
///
/// impl<T: Provided + WebhookSource> TriggerHost<T> for WebhookHost {
///     type Payload = WebhookEvent;
///
///     fn handle_step(
///         target: Arc<T>,
///     ) -> BoxFuture<'static, TriggerTransition<Self::Payload>> {
///         Box::pin(async move {
///             match target.poll_webhook().await {
///                 Some(event) => TriggerTransition::Next(event),
///                 None => TriggerTransition::Stop,
///             }
///         })
///     }
/// }
/// ```
pub trait TriggerHost<T: Send + Sync + 'static>: Sized {
    /// The business payload type carried by each event.
    type Payload: Send + Sync + 'static;

    /// **Policy**: Define how to wait for the next event and what transition
    /// to make.
    ///
    /// This is the **only method** most trigger hosts need to implement.
    /// The default engine calls this in a loop and handles tracing, ID
    /// issuance, dispatch, and shutdown automatically.
    ///
    /// # Return values
    /// - `TriggerTransition::Next(payload)` — dispatch and loop again.
    /// - `TriggerTransition::Reload(payload)` — dispatch, then idle for reload.
    /// - `TriggerTransition::Stop` — exit the loop cleanly.
    fn handle_step(target: Arc<T>) -> BoxFuture<'static, TriggerTransition<Self::Payload>>;

    /// **Engine**: Start the trigger's event loop as a long-running service.
    ///
    /// The default implementation provides a complete event loop that:
    /// 1. Calls `handle_step` in a `tokio::select!` with shutdown monitoring.
    /// 2. Dispatches payloads through a tracing-instrumented handler pipeline.
    /// 3. Issues monotonically increasing instance sequence IDs.
    /// 4. On `Reload`, idles via `wait_shutdown()` so the framework's
    ///    `ServiceWatcher` can restart us when dependencies change.
    ///
    /// Override this only for hosts that cannot fit the `handle_step` model
    /// (e.g., `CronHost` which relies on external scheduler callbacks).
    fn run_as_service(
        name: String,
        target: Arc<T>,
        handler: TriggerHandler<Self::Payload>,
        _token: CancellationToken,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async move {
            let service_id = current_service_id();
            let instance_counter = AtomicU64::new(0);

            while !context::is_shutdown() {
                // Race: policy step vs shutdown signal
                let transition = tokio::select! {
                    t = Self::handle_step(target.clone()) => t,
                    _ = context::wait_shutdown() => {
                        info!("Trigger '{}' received shutdown, exiting", name);
                        break;
                    }
                };

                match transition {
                    TriggerTransition::Next(payload) => {
                        dispatch_event(&name, service_id, &instance_counter, payload, &handler)
                            .await;
                    }
                    TriggerTransition::Reload(payload) => {
                        dispatch_event(&name, service_id, &instance_counter, payload, &handler)
                            .await;
                        // Idle until the framework's ServiceWatcher aborts us
                        // for a dependency-driven reload. This keeps the
                        // service status as "Running" in the meantime.
                        info!("Trigger '{}' entering reload-wait state", name);
                        context::wait_shutdown().await;
                        break;
                    }
                    TriggerTransition::Stop => {
                        info!("Trigger '{}' stopping", name);
                        break;
                    }
                }
            }

            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// Engine internals — dispatch and tracing helpers
// ---------------------------------------------------------------------------

/// Attempts to retrieve the current service's `ServiceId` from the task-local
/// context. Falls back to `ServiceId(0)` if called outside a service scope.
fn current_service_id() -> ServiceId {
    context::identity::CURRENT_SERVICE
        .try_with(|identity| identity.service_id)
        .unwrap_or(ServiceId::new(0))
}

/// Generates a globally unique message ID for each trigger event.
pub(crate) fn generate_message_id() -> String {
    #[cfg(feature = "uuid-trigger-ids")]
    {
        uuid::Uuid::new_v4().to_string()
    }
    #[cfg(not(feature = "uuid-trigger-ids"))]
    {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("msg-{}", id)
    }
}

/// Dispatches a single trigger event through the tracing + handler pipeline.
///
/// This is the engine's internal dispatch primitive. It handles:
/// 1. Building a [`TriggerContext`] with traceability metadata.
/// 2. Creating a tracing span for the invocation.
/// 3. Invoking the user handler and logging any errors.
async fn dispatch_event<P: Send + Sync + 'static>(
    name: &str,
    service_id: ServiceId,
    instance_counter: &AtomicU64,
    payload: P,
    handler: &TriggerHandler<P>,
) {
    let seq = instance_counter.fetch_add(1, Ordering::Relaxed);
    let ctx = TriggerContext {
        service_id,
        instance_seq: seq,
        message: TriggerMessage {
            message_id: generate_message_id(),
            source_id: service_id,
            timestamp: Utc::now(),
            payload,
        },
    };
    let instance_id = ctx.trigger_instance_id();
    let message_id = ctx.message.message_id.clone();
    let span = tracing::info_span!("trigger", %name, %instance_id, %message_id);
    let h = handler.clone();
    async move {
        info!("Trigger fired");
        if let Err(e) = h(ctx).await {
            error!("Trigger error: {:?}", e);
        }
    }
    .instrument(span)
    .await;
}

// ===========================================================================
// Built-in Trigger Host aliases (the `TT` module)
// ===========================================================================

/// Short aliases for built-in trigger hosts.
///
/// This module re-exports the concrete host structs under user-friendly names
/// so that `#[trigger]` attributes read naturally:
///
/// ```rust,ignore
/// use service_daemon::TT::*;
///
/// #[trigger(Notify(MySignal))]
/// async fn on_signal() -> anyhow::Result<()> { Ok(()) }
///
/// #[trigger(Queue(MyQueue))]
/// async fn on_message(payload: String) -> anyhow::Result<()> { Ok(()) }
/// ```
#[allow(non_snake_case)]
pub mod TT {
    // Signal-based (payload: ())
    pub use crate::core::triggers::SignalHost;
    pub use crate::core::triggers::SignalHost as Custom;
    pub use crate::core::triggers::SignalHost as Event;
    pub use crate::core::triggers::SignalHost as Notify;
    pub use crate::core::triggers::SignalHost as Signal;

    // Broadcast Queue (payload: T from the queue's item_type)
    pub use crate::core::triggers::TopicHost as BQueue;
    pub use crate::core::triggers::TopicHost as BroadcastQueue;
    pub use crate::core::triggers::TopicHost as Queue;

    // Load-Balancing Queue (payload: T from the queue's item_type)
    pub use crate::core::triggers::LBTopicHost as LBQueue;
    pub use crate::core::triggers::LBTopicHost as LoadBalancingQueue;

    // Cron-based scheduled trigger (payload: ())
    #[cfg(feature = "cron")]
    pub use crate::core::triggers::CronHost;
    #[cfg(feature = "cron")]
    pub use crate::core::triggers::CronHost as Cron;

    // State-watch trigger (payload: ())
    pub use crate::core::triggers::WatchHost;
    pub use crate::core::triggers::WatchHost as State;
    pub use crate::core::triggers::WatchHost as Watch;
}
