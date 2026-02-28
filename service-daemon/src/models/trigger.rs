//! Trigger system models: `TriggerHost` trait, context types, and built-in aliases.
//!
//! The trigger system is built around the **Policy-Engine** separation:
//!
//! - **Policy** (`handle_step`): Defined by each trigger host. It only cares about
//!   *"how to wait for the next event"* and returns a [`TriggerTransition`].
//! - **Engine** (`run_as_service` default impl --> [`TriggerRunner`](crate::core::trigger_runner::TriggerRunner)):
//!   Manages the event loop, tracing, retry, middleware pipeline, and graceful
//!   shutdown. Host implementors get this for free.
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
//!
//!     fn setup(target: Arc<T>) -> BoxFuture<'static, anyhow::Result<Self>> {
//!         Box::pin(async { Ok(MyHost) })
//!     }
//!
//!     fn handle_step<'a>(&'a mut self, target: &'a Arc<T>)
//!         -> BoxFuture<'a, TriggerTransition<Self::Payload>>
//!     {
//!         Box::pin(async move {
//!             let event = target.wait_for_event().await;
//!             TriggerTransition::Next(event)
//!         })
//!     }
//! }
//! ```
//!
//! No changes to the macro crate are needed -- the `#[trigger]` macro resolves
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
use tokio_util::sync::CancellationToken;

use super::service::ServiceId;
use crate::core::context;

// ---------------------------------------------------------------------------
// Payload extraction helper (used by macro-generated code)
// ---------------------------------------------------------------------------

/// Clones a payload out of its `Arc` wrapper for handlers that receive
/// the payload by value (i.e. `fn handler(data: T)` instead of
/// `fn handler(data: Arc<T>)`).
///
/// # Purpose
///
/// The `#[trigger]` macro generates calls to this function when the
/// user's handler parameter is a bare `T` (not `Arc<T>`). By routing
/// through a named function, the compiler error when `T: Clone` is
/// missing will reference this function name, making it clear **why**
/// `Clone` is required:
///
/// ```text
/// error[E0277]: the trait bound `MyType: Clone` is not satisfied
///   --> src/trigger_handlers.rs:10:1
///    |
///    = note: required by `service_daemon::trigger_clone_payload`
///    = help: wrap the payload in `Arc<MyType>` to avoid cloning
/// ```
///
/// If you see this error, you have two options:
/// 1. Derive `Clone` for your payload type.
/// 2. Change the handler parameter to `Arc<MyType>` for zero-copy access.
#[doc(hidden)]
#[inline(always)]
pub fn trigger_clone_payload<T: Clone>(arc_payload: &T) -> T {
    arc_payload.clone()
}

// ---------------------------------------------------------------------------
// TriggerMessage -- traceable event payload (the "stone" in the ripple model)
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
    ///
    /// Wrapped in `Arc` at the framework level so that the retry host
    /// can share the payload across multiple attempts without requiring
    /// the business type `P` to implement `Clone`.
    pub payload: Arc<P>,
}

// ---------------------------------------------------------------------------
// TriggerContext -- runtime context available to every trigger handler
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
// TriggerHandler -- unified async handler signature
// ---------------------------------------------------------------------------

/// The canonical function signature for trigger event handlers.
///
/// Every trigger host invokes a handler of this shape, providing a
/// `TriggerContext` with full traceability information.
pub type TriggerHandler<P> = Arc<
    dyn Fn(TriggerContext<P>) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync + 'static,
>;

// ---------------------------------------------------------------------------
// TriggerTransition -- lifecycle handshake protocol
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
    /// Used by streaming triggers (Signal, Queue) that process
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
// TriggerHost -- the sole extension point for all trigger types
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
pub trait TriggerHost<T: Send + Sync + 'static>: Sized + Send {
    /// The business payload type carried by each event.
    ///
    /// Does **not** require `Clone`. The framework wraps the payload in
    /// `Arc<P>` internally, so retries only clone the pointer. If your
    /// handler receives a bare `T` (not `Arc<T>`), the macro will
    /// auto-clone -- in that case `T` must implement `Clone`.
    type Payload: Send + Sync + 'static;

    /// **Setup**: One-time initialization for the host.
    ///
    /// Called once when the trigger service starts. Use this to acquire
    /// resources (subscribers, scheduler jobs, etc.) that persist across
    /// `handle_step` iterations.
    ///
    /// Hosts that need no initialization can simply return `Ok(Self)`.
    fn setup(target: Arc<T>) -> BoxFuture<'static, anyhow::Result<Self>>;

    /// **Policy**: Define how to wait for the next event and what transition
    /// to make.
    ///
    /// This is the **only method** most trigger hosts need to implement.
    /// The default engine calls this in a loop and handles tracing, ID
    /// issuance, dispatch, and shutdown automatically.
    ///
    /// # Return values
    /// - `TriggerTransition::Next(payload)` -- dispatch and loop again.
    /// - `TriggerTransition::Reload(payload)` -- dispatch, then idle for reload.
    /// - `TriggerTransition::Stop` -- exit the loop cleanly.
    fn handle_step<'a>(
        &'a mut self,
        target: &'a Arc<T>,
    ) -> BoxFuture<'a, TriggerTransition<Self::Payload>>;

    /// **Scaling**: Declare whether this trigger type needs elastic scaling.
    ///
    /// Returns `None` by default, which means the trigger runner will
    /// dispatch events serially (single permit, no scale monitor).
    ///
    /// Streaming trigger templates (e.g. `TopicHost` for `Queue`) should
    /// override this to return `Some(ScalingPolicy::default())`, which
    /// enables the pressure-based auto-scaler.
    ///
    /// Users can override the template's default via
    /// [`ServiceDaemonBuilder::with_trigger_config`].
    fn scaling_policy() -> Option<crate::models::policy::ScalingPolicy> {
        None
    }

    /// **Engine**: Start the trigger's event loop as a long-running service.
    ///
    /// The default implementation provides a complete event loop that:
    /// 1. Calls `setup` once to initialize the host.
    /// 2. Calls `handle_step` in a `tokio::select!` with shutdown monitoring.
    /// 3. Dispatches payloads through a middleware-instrumented handler pipeline.
    /// 4. Issues monotonically increasing instance sequence IDs.
    /// 5. On `Reload`, idles via `wait_shutdown()` so the framework's
    ///    `ServiceWatcher` can restart us when dependencies change.
    ///
    /// Override this only for hosts that cannot fit the `setup` + `handle_step`
    /// model.
    fn run_as_service(
        name: String,
        target: Arc<T>,
        handler: TriggerHandler<Self::Payload>,
        _token: CancellationToken,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async move {
            let service_id = current_service_id();

            let mut host = Self::setup(target.clone()).await?;

            // Priority chain: user override > template declaration > None
            let scaling =
                crate::core::context::trigger_config::<crate::models::policy::ScalingPolicy>()
                    .or_else(|| Self::scaling_policy());

            let runner = crate::core::trigger_runner::TriggerRunner::new(
                name,
                service_id,
                handler,
                crate::models::policy::RestartPolicy::default(),
                scaling,
            );

            runner.run_with_host::<T, Self>(&mut host, target).await
        })
    }
}

// ---------------------------------------------------------------------------
// Engine internals -- dispatch and tracing helpers
// ---------------------------------------------------------------------------

/// Attempts to retrieve the current service's `ServiceId` from the task-local
/// context. Falls back to `ServiceId(0)` if called outside a service scope.
fn current_service_id() -> ServiceId {
    context::identity::CURRENT_SERVICE
        .try_with(|identity| identity.service_id)
        .unwrap_or(ServiceId::new(0))
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
