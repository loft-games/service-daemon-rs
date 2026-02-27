//! Trigger middleware infrastructure.
//!
//! This module provides [`TriggerMiddleware`], a trait that allows pluggable
//! logic to run before and after each trigger event dispatch. Combined with
//! [`TriggerRunner`], it encapsulates the event loop, signal handling,
//! middleware pipeline, tracing, and retry logic that were previously split
//! across `run_as_service`, `dispatch_event`, and `TriggerInvocation`.
//!
//! # Architecture
//!
//! ```text
//!   handle_step --> TriggerRunner.run_with_host()
//!                       |
//!                       v
//!                   dispatch_with_middleware(payload)
//!                       +-- middleware.before_dispatch()
//!                       +-- invoke_handler_with_retry()
//!                       |       +-- build TriggerContext
//!                       |       +-- call handler
//!                       |       +-- backoff on failure
//!                       +-- middleware.after_dispatch()
//! ```
//!
//! The `TriggerRunner` owns the `select!` + shutdown logic, so that trigger
//! hosts only need to implement `handle_step` and get a clean, flat event loop.

use chrono::Utc;
use futures::future::BoxFuture;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{Instrument, info, warn};

use crate::core::context;
use crate::models::policy::{BackoffController, RestartPolicy};
use crate::models::service::ServiceId;
use crate::models::trigger::{
    TriggerContext, TriggerHandler, TriggerHost, TriggerMessage, TriggerTransition,
};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Generates a globally unique message ID for each trigger event.
///
/// This is also called by the public `context::generate_message_id()` API.
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

// ---------------------------------------------------------------------------
// TriggerMiddleware trait
// ---------------------------------------------------------------------------

/// Outcome returned by [`TriggerMiddleware::after_dispatch`].
///
/// Tells the runner whether to continue the event loop normally or take
/// a special action such as skipping the next iteration.
pub enum MiddlewareAction {
    /// Continue the event loop normally.
    Continue,
    /// Stop the trigger's event loop.
    Stop,
}

/// A pluggable hook that wraps each trigger event dispatch.
///
/// Middlewares are invoked in registration order for `before_dispatch` and
/// in **reverse** order for `after_dispatch` (onion model).
///
/// # Current Usage
///
/// The framework internally uses this to attach tracing spans. In the future,
/// user-facing middleware registration (via macro attributes or builder API)
/// will allow custom logic such as rate limiting, authentication, and metrics.
pub trait TriggerMiddleware: Send + Sync {
    /// Called before the event payload is dispatched to the handler.
    ///
    /// Returning `Err` will skip dispatch and proceed to `after_dispatch`.
    fn before_dispatch(&self, trigger_name: &str) -> BoxFuture<'_, anyhow::Result<()>>;

    /// Called after dispatch completes (or is skipped due to `before_dispatch` error).
    fn after_dispatch(
        &self,
        trigger_name: &str,
        result: &anyhow::Result<()>,
    ) -> BoxFuture<'_, MiddlewareAction>;
}

// ---------------------------------------------------------------------------
// TriggerRunner -- the flat event-loop driver
// ---------------------------------------------------------------------------

/// Encapsulates the trigger event loop, signal handling, and middleware pipeline.
///
/// `TriggerRunner` replaces the inline `while/select!/match` structure that was
/// previously embedded in `TriggerHost::run_as_service`. It also absorbs the
/// responsibilities of `dispatch_event` and `TriggerInvocation`:
///
/// - **Context construction** (seq ID, message ID)
/// - **Tracing instrumentation** (`info_span!`)
/// - **Retry with backoff** (exponential backoff on handler failure)
///
/// Each concern lives in its own private method, keeping individual function
/// complexity low while centralising all dispatch logic.
///
/// # Type Parameters
///
/// - `P`: The payload type produced by `handle_step`.
pub struct TriggerRunner<P: Send + Sync + 'static> {
    /// Human-readable name of this trigger service.
    name: String,
    /// The `ServiceId` of the trigger service.
    service_id: ServiceId,
    /// Monotonically increasing instance counter for tracing.
    instance_counter: AtomicU64,
    /// The user's event handler.
    handler: TriggerHandler<P>,
    /// Registered middleware chain (executed in order).
    middlewares: Vec<Box<dyn TriggerMiddleware>>,
}

impl<P: Send + Sync + 'static> TriggerRunner<P> {
    /// Create a new runner with the given name and handler.
    ///
    /// The built-in [`TracingMiddleware`] is automatically registered as the
    /// first middleware, providing per-dispatch-cycle logging for free.
    pub fn new(name: String, service_id: ServiceId, handler: TriggerHandler<P>) -> Self {
        Self {
            name,
            service_id,
            instance_counter: AtomicU64::new(0),
            handler,
            middlewares: vec![Box::new(TracingMiddleware)],
        }
    }

    /// Register a middleware to the pipeline.
    ///
    /// Middlewares are invoked in the order they are added for
    /// `before_dispatch`, and in reverse order for `after_dispatch`.
    pub fn with_middleware(mut self, middleware: Box<dyn TriggerMiddleware>) -> Self {
        self.middlewares.push(middleware);
        self
    }

    /// Run the event loop with a pre-initialized host instance.
    ///
    /// Takes a mutable reference to an already-constructed host. The host's
    /// `handle_step(&mut self, &target)` is called in each iteration, allowing
    /// hosts to maintain state across iterations without relying on `shelve`.
    pub async fn run_with_host<T, H>(&self, host: &mut H, target: Arc<T>) -> anyhow::Result<()>
    where
        T: Send + Sync + 'static,
        H: TriggerHost<T, Payload = P>,
    {
        while !context::is_shutdown() {
            // Race: policy step vs shutdown signal
            let transition = tokio::select! {
                t = host.handle_step(&target) => t,
                _ = context::wait_shutdown() => {
                    info!("Trigger '{}' received shutdown, exiting", self.name);
                    break;
                }
            };

            match transition {
                TriggerTransition::Next(payload) => {
                    self.dispatch_with_middleware(payload).await;
                }
                TriggerTransition::Reload(payload) => {
                    self.dispatch_with_middleware(payload).await;
                    // Idle until the framework's ServiceWatcher aborts us
                    info!("Trigger '{}' entering reload-wait state", self.name);
                    context::wait_shutdown().await;
                    break;
                }
                TriggerTransition::Stop => {
                    info!("Trigger '{}' stopping", self.name);
                    break;
                }
            }
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Dispatch pipeline (private methods)
    // -----------------------------------------------------------------------

    /// Dispatch a single event through the middleware pipeline, then to the handler.
    ///
    /// Orchestrates the before/after middleware hooks around the core
    /// retry-capable handler invocation. Each concern is a separate method.
    async fn dispatch_with_middleware(&self, payload: P) {
        // --- before_dispatch (forward order) ---
        for mw in &self.middlewares {
            if let Err(e) = mw.before_dispatch(&self.name).await {
                tracing::warn!(
                    "Middleware before_dispatch failed for '{}': {:?}",
                    self.name,
                    e
                );
                let result = Err(e);
                for mw_rev in self.middlewares.iter().rev() {
                    mw_rev.after_dispatch(&self.name, &result).await;
                }
                return;
            }
        }

        // --- core dispatch (context + tracing + retry) ---
        let result = self.dispatch_core(payload).await;

        // --- after_dispatch (reverse order) ---
        for mw in self.middlewares.iter().rev() {
            let action = mw.after_dispatch(&self.name, &result).await;
            if matches!(action, MiddlewareAction::Stop) {
                info!("Middleware requested stop for trigger '{}'", self.name);
                break;
            }
        }
    }

    /// Core dispatch: construct context, create tracing span, invoke handler
    /// with retry.
    ///
    /// This method absorbs the responsibilities of the former `dispatch_event`
    /// function and `TriggerInvocation` struct, but delegates the retry loop
    /// to [`Self::invoke_handler_with_retry`].
    async fn dispatch_core(&self, payload: P) -> anyhow::Result<()> {
        let seq = self.instance_counter.fetch_add(1, Ordering::Relaxed);
        let message_id = generate_message_id();

        let span = tracing::info_span!(
            "trigger",
            name = %self.name,
            instance_id = %format!("{}:{}", self.service_id, seq),
            %message_id,
        );

        let payload = Arc::new(payload);
        self.invoke_handler_with_retry(seq, message_id, payload)
            .instrument(span)
            .await
    }

    /// Execute the handler with exponential backoff retry on failure.
    ///
    /// On success, returns `Ok(())` immediately. On failure, waits using
    /// exponential backoff before retrying. Respects shutdown signals during
    /// the backoff wait.
    ///
    /// The payload is shared via `Arc`, so each retry only clones the pointer
    /// (not the business data).
    async fn invoke_handler_with_retry(
        &self,
        instance_seq: u64,
        message_id: String,
        payload: Arc<P>,
    ) -> anyhow::Result<()> {
        info!("Trigger fired");

        let mut backoff = BackoffController::new(RestartPolicy::default());

        loop {
            let ctx = TriggerContext {
                service_id: self.service_id,
                instance_seq,
                message: TriggerMessage {
                    message_id: message_id.clone(),
                    source_id: self.service_id,
                    timestamp: Utc::now(),
                    payload: payload.clone(), // Arc clone -- cheap pointer copy
                },
            };

            match (self.handler)(ctx).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    warn!(
                        attempt = backoff.attempt_count() + 1,
                        error = %e,
                        "Trigger handler failed, scheduling retry"
                    );
                    backoff.record_failure();

                    // Check if shutdown was requested before waiting
                    if context::is_shutdown() {
                        warn!("Trigger handler retry aborted due to shutdown");
                        return Err(e);
                    }

                    // Use interruptible sleep via context::sleep
                    if !context::sleep(backoff.current_delay()).await {
                        warn!("Trigger handler retry interrupted by shutdown");
                        return Err(e);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Built-in: TracingMiddleware
// ---------------------------------------------------------------------------

/// Built-in middleware that logs dispatch lifecycle events.
///
/// This is automatically registered as the first middleware in every
/// `TriggerRunner` created via [`TriggerRunner::new`]. It provides
/// per-dispatch-cycle logging that complements the per-event tracing
/// span created inside [`TriggerRunner::dispatch_core`].
///
/// # Log output
///
/// ```text
/// DEBUG trigger_dispatch{trigger="my_trigger"}: Middleware: dispatching event
/// DEBUG trigger_dispatch{trigger="my_trigger"}: Middleware: dispatch completed
/// ```
pub(crate) struct TracingMiddleware;

impl TriggerMiddleware for TracingMiddleware {
    fn before_dispatch(&self, trigger_name: &str) -> BoxFuture<'_, anyhow::Result<()>> {
        let name = trigger_name.to_owned();
        Box::pin(async move {
            tracing::debug!(trigger = %name, "Middleware: dispatching event");
            Ok(())
        })
    }

    fn after_dispatch(
        &self,
        trigger_name: &str,
        result: &anyhow::Result<()>,
    ) -> BoxFuture<'_, MiddlewareAction> {
        let is_ok = result.is_ok();
        let trigger_name = trigger_name.to_owned();
        Box::pin(async move {
            if is_ok {
                tracing::debug!(trigger = %trigger_name, "Middleware: dispatch completed");
            } else {
                tracing::warn!(trigger = %trigger_name, "Middleware: dispatch failed");
            }
            MiddlewareAction::Continue
        })
    }
}
