//! Trigger interceptor infrastructure.
//!
//! This module provides [`TriggerInterceptor`], a composable middleware trait
//! that gives each interceptor full control over the dispatch lifecycle. Combined
//! with [`TriggerRunner`], it encapsulates the event loop, signal handling,
//! interceptor pipeline, tracing, and retry logic.
//!
//! # Architecture (Onion Model)
//!
//! ```text
//!   handle_step --> TriggerRunner.run_with_host()
//!                       |
//!                       v
//!                   dispatch(payload)
//!                       +-- TracingInterceptor.intercept(ctx, next)
//!                             +-- RetryInterceptor.intercept(ctx, next)
//!                                   +-- handler(TriggerContext)
//! ```
//!
//! Each interceptor receives a [`DispatchContext`] by value and a `next`
//! callback. The interceptor decides **if, when, and how many times** to
//! call `next`, enabling patterns like retry, tracing spans, rate limiting,
//! and circuit breaking — all as composable, user-extensible layers.
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
// DispatchContext -- the data envelope flowing through the interceptor chain
// ---------------------------------------------------------------------------

/// Context passed through the interceptor chain for each dispatch cycle.
///
/// Contains all the metadata needed to construct a [`TriggerContext`] for the
/// final handler invocation. Interceptors receive this by value (owned) and
/// pass it forward to `next`, which avoids mutable borrow conflicts across
/// nested closures.
///
/// # Ownership Model
///
/// The context is moved through the chain: each interceptor receives ownership,
/// may inspect or modify fields, then passes it to `next`. This eliminates
/// lifetime entanglement between interceptor layers.
pub struct DispatchContext<P> {
    /// The `ServiceId` of the trigger service.
    pub service_id: ServiceId,
    /// Monotonically increasing sequence number within this trigger service.
    pub instance_seq: u64,
    /// Globally unique identifier for this event instance.
    pub message_id: String,
    /// Human-readable name of this trigger service (for logging/tracing).
    pub trigger_name: String,
    /// The business payload, wrapped in `Arc` for cheap cloning across retries.
    pub payload: Arc<P>,
    /// The user's event handler (needed at the terminal node of the chain).
    pub handler: TriggerHandler<P>,
}

// ---------------------------------------------------------------------------
// TriggerInterceptor -- the composable middleware trait
// ---------------------------------------------------------------------------

/// The remainder of the interceptor chain after the current interceptor.
///
/// An interceptor calls `next(ctx)` to invoke the rest of the chain. It may:
/// - Call `next` exactly once (pass-through).
/// - Call `next` zero times (short-circuit / reject).
/// - Call `next` multiple times (retry).
/// - Wrap the `next` call in a tracing span, timer, or other context.
pub type Next<'a, P> =
    Box<dyn FnOnce(DispatchContext<P>) -> BoxFuture<'a, anyhow::Result<()>> + Send + 'a>;

/// A composable interceptor that wraps trigger event dispatch.
///
/// Unlike the previous `TriggerMiddleware` (which was a passive observer with
/// `before_dispatch` / `after_dispatch` hooks), `TriggerInterceptor` follows
/// the **onion model**: each interceptor fully wraps the next layer and has
/// complete control over the dispatch lifecycle.
///
/// # Type Parameter `P`
///
/// The payload type is bound at the trait level, making the trait object-safe
/// within a specific `TriggerRunner<P>` instance. This is the "semi-static
/// dispatch" design: payload types are statically checked, while the
/// interceptor chain is dynamically composable via `Vec<Box<dyn ...>>`.
///
/// # Writing a Generic Interceptor
///
/// Interceptors that don't care about the payload type can use a blanket impl:
///
/// ```rust,ignore
/// struct MyInterceptor;
///
/// impl<P: Send + Sync + 'static> TriggerInterceptor<P> for MyInterceptor {
///     fn intercept<'a>(
///         &'a self,
///         ctx: DispatchContext<P>,
///         next: Next<'a, P>,
///     ) -> BoxFuture<'a, anyhow::Result<()>> {
///         Box::pin(async move {
///             // ... pre-processing ...
///             let result = next(ctx).await;
///             // ... post-processing ...
///             result
///         })
///     }
/// }
/// ```
pub trait TriggerInterceptor<P: Send + Sync + 'static>: Send + Sync {
    /// Execute this interceptor's logic, optionally calling `next` to continue
    /// the chain.
    ///
    /// # Arguments
    ///
    /// * `ctx` - The dispatch context (owned). Pass it to `next` to continue.
    /// * `next` - A boxed closure representing the rest of the interceptor chain.
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, or an error that will propagate back through the
    /// chain (each outer interceptor can catch and handle errors).
    fn intercept<'a>(
        &'a self,
        ctx: DispatchContext<P>,
        next: Next<'a, P>,
    ) -> BoxFuture<'a, anyhow::Result<()>>;
}

// ---------------------------------------------------------------------------
// TriggerRunner -- the flat event-loop driver
// ---------------------------------------------------------------------------

/// Encapsulates the trigger event loop, signal handling, and interceptor pipeline.
///
/// `TriggerRunner` replaces the inline `while/select!/match` structure that was
/// previously embedded in `TriggerHost::run_as_service`. It uses the
/// interceptor architecture where each cross-cutting concern (tracing, retry)
/// is a composable [`TriggerInterceptor`] layer.
///
/// # Interceptor Chain
///
/// The chain is built from the registered interceptors plus a terminal handler
/// node. On each dispatch, the runner constructs the chain from back to front:
///
/// ```text
/// interceptors[0].intercept(ctx,
///   interceptors[1].intercept(ctx,
///     ... terminal_handler(ctx) ...))
/// ```
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
    /// Registered interceptor chain (executed in registration order, onion model).
    interceptors: Vec<Box<dyn TriggerInterceptor<P>>>,
}

impl<P: Send + Sync + 'static> TriggerRunner<P> {
    /// Create a new runner with the given name and handler.
    ///
    /// The built-in [`TracingInterceptor`] and [`RetryInterceptor`] are
    /// automatically registered, providing per-dispatch tracing and
    /// exponential-backoff retry for free.
    ///
    /// The default interceptor order is:
    /// 1. `TracingInterceptor` — wraps everything in a tracing span
    /// 2. `RetryInterceptor` — retries the inner chain on failure
    /// 3. (user interceptors added via `with_interceptor`)
    /// 4. Terminal handler node (implicit)
    pub fn new(name: String, service_id: ServiceId, handler: TriggerHandler<P>) -> Self {
        Self {
            name,
            service_id,
            instance_counter: AtomicU64::new(0),
            handler,
            interceptors: vec![Box::new(TracingInterceptor), Box::new(RetryInterceptor)],
        }
    }

    /// Register an interceptor to the pipeline.
    ///
    /// Interceptors are invoked in registration order (onion model: first
    /// registered = outermost layer). The built-in `TracingInterceptor` and
    /// `RetryInterceptor` are always the first two layers.
    pub fn with_interceptor(mut self, interceptor: Box<dyn TriggerInterceptor<P>>) -> Self {
        self.interceptors.push(interceptor);
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
                    self.dispatch(payload).await;
                }
                TriggerTransition::Reload(payload) => {
                    self.dispatch(payload).await;
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
    // Dispatch pipeline
    // -----------------------------------------------------------------------

    /// Dispatch a single event through the interceptor chain.
    ///
    /// Constructs a [`DispatchContext`] and builds the interceptor call chain
    /// from back to front, then invokes the outermost interceptor.
    async fn dispatch(&self, payload: P) {
        let seq = self.instance_counter.fetch_add(1, Ordering::Relaxed);
        let message_id = generate_message_id();

        let ctx = DispatchContext {
            service_id: self.service_id,
            instance_seq: seq,
            message_id,
            trigger_name: self.name.clone(),
            payload: Arc::new(payload),
            handler: self.handler.clone(),
        };

        // Build the chain from back to front, starting with the terminal handler
        let result = self.invoke_chain(ctx).await;

        if let Err(e) = result {
            warn!(
                trigger = %self.name,
                error = %e,
                "Dispatch chain completed with error"
            );
        }
    }

    /// Build and invoke the interceptor chain.
    ///
    /// Starting from the terminal handler node (which converts `DispatchContext`
    /// into `TriggerContext` and calls the user handler), each interceptor is
    /// wrapped around the previous one in reverse order.
    ///
    /// The lifetime `'a` is tied to `&self`, allowing interceptor references
    /// to be captured in the closures without requiring `'static`.
    async fn invoke_chain(&self, ctx: DispatchContext<P>) -> anyhow::Result<()> {
        // Terminal node: convert DispatchContext -> TriggerContext and call handler
        let terminal: Next<'_, P> = Box::new(|ctx: DispatchContext<P>| {
            Box::pin(async move {
                let trigger_ctx = TriggerContext {
                    service_id: ctx.service_id,
                    instance_seq: ctx.instance_seq,
                    message: TriggerMessage {
                        message_id: ctx.message_id,
                        source_id: ctx.service_id,
                        timestamp: Utc::now(),
                        payload: ctx.payload,
                    },
                };
                (ctx.handler)(trigger_ctx).await
            }) as BoxFuture<'_, anyhow::Result<()>>
        });

        // Fold interceptors from back to front, wrapping each around the previous
        let chain = self
            .interceptors
            .iter()
            .rev()
            .fold(terminal, |next, interceptor| {
                Box::new(move |ctx| interceptor.intercept(ctx, next))
            });

        // Invoke the outermost interceptor (which cascades inward)
        chain(ctx).await
    }
}

// ---------------------------------------------------------------------------
// Built-in: TracingInterceptor
// ---------------------------------------------------------------------------

/// Built-in interceptor that wraps each dispatch cycle in a tracing span.
///
/// This is automatically registered as the **outermost** interceptor in every
/// `TriggerRunner`. It creates an `info_span!("trigger", ...)` that covers
/// the entire dispatch lifecycle, including retries.
///
/// # Span Fields
///
/// - `name`: The trigger service name.
/// - `instance_id`: `{service_id}:{sequence}` for correlating invocations.
/// - `message_id`: The globally unique event identifier.
///
/// # Log Output
///
/// ```text
/// INFO trigger{name="my_trigger" instance_id="svc#1:0" message_id="msg-0"}: Trigger fired
/// ```
pub(crate) struct TracingInterceptor;

impl<P: Send + Sync + 'static> TriggerInterceptor<P> for TracingInterceptor {
    fn intercept<'a>(
        &'a self,
        ctx: DispatchContext<P>,
        next: Next<'a, P>,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let span = tracing::info_span!(
                "trigger",
                name = %ctx.trigger_name,
                instance_id = %format!("{}:{}", ctx.service_id, ctx.instance_seq),
                message_id = %ctx.message_id,
            );

            info!(parent: &span, "Trigger fired");

            next(ctx).instrument(span).await
        })
    }
}

// ---------------------------------------------------------------------------
// Built-in: RetryInterceptor
// ---------------------------------------------------------------------------

/// Built-in interceptor that retries the inner chain on failure with
/// exponential backoff.
///
/// Registered as the second interceptor (inside `TracingInterceptor`), so
/// that retry attempts are grouped within the same tracing span.
///
/// # Retry Behavior
///
/// - Uses [`BackoffController`] with the default [`RestartPolicy`].
/// - On handler failure, logs a warning and waits before retrying.
/// - Respects shutdown signals during the backoff wait period.
/// - On success, returns `Ok(())` immediately (no further retries).
///
/// # Payload Sharing
///
/// The payload is wrapped in `Arc<P>` at the `DispatchContext` level, so
/// each retry only clones the `Arc` pointer (not the business data). The
/// `DispatchContext` itself is reconstructed for each retry attempt from
/// the shared fields.
pub(crate) struct RetryInterceptor;

impl<P: Send + Sync + 'static> TriggerInterceptor<P> for RetryInterceptor {
    fn intercept<'a>(
        &'a self,
        ctx: DispatchContext<P>,
        next: Next<'a, P>,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let mut backoff = BackoffController::new(RestartPolicy::default());

            // Preserve shared fields for reconstruction across retries
            let service_id = ctx.service_id;
            let instance_seq = ctx.instance_seq;
            let message_id = ctx.message_id;
            let trigger_name = ctx.trigger_name;
            let payload = ctx.payload;
            let handler = ctx.handler;

            // First attempt: use the original `next` closure
            let first_ctx = DispatchContext {
                service_id,
                instance_seq,
                message_id: message_id.clone(),
                trigger_name: trigger_name.clone(),
                payload: payload.clone(),
                handler: handler.clone(),
            };

            match next(first_ctx).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    warn!(
                        attempt = backoff.attempt_count() + 1,
                        error = %e,
                        "Trigger handler failed, scheduling retry"
                    );
                    backoff.record_failure();

                    if context::is_shutdown() {
                        warn!("Trigger handler retry aborted due to shutdown");
                        return Err(e);
                    }

                    if !context::sleep(backoff.current_delay()).await {
                        warn!("Trigger handler retry interrupted by shutdown");
                        return Err(e);
                    }
                }
            }

            // Subsequent retries: call the handler directly (no need to re-enter
            // interceptors below us, since retry IS the re-entry mechanism)
            loop {
                let retry_ctx = TriggerContext {
                    service_id,
                    instance_seq,
                    message: TriggerMessage {
                        message_id: message_id.clone(),
                        source_id: service_id,
                        timestamp: Utc::now(),
                        payload: payload.clone(),
                    },
                };

                match (handler.clone())(retry_ctx).await {
                    Ok(()) => return Ok(()),
                    Err(e) => {
                        warn!(
                            attempt = backoff.attempt_count() + 1,
                            error = %e,
                            "Trigger handler failed, scheduling retry"
                        );
                        backoff.record_failure();

                        if context::is_shutdown() {
                            warn!("Trigger handler retry aborted due to shutdown");
                            return Err(e);
                        }

                        if !context::sleep(backoff.current_delay()).await {
                            warn!("Trigger handler retry interrupted by shutdown");
                            return Err(e);
                        }
                    }
                }
            }
        })
    }
}
