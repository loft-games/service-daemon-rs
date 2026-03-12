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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use tokio::sync::Semaphore;
use tracing::{Instrument, info, warn};

use crate::core::context;
use crate::models::policy::{BackoffController, RestartPolicy, ScalingPolicy};
use crate::models::service::ServiceId;
use crate::models::trigger::{
    TriggerContext, TriggerHandler, TriggerHost, TriggerMessage, TriggerTransition,
};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Generates a globally unique, time-ordered message ID for each trigger event.
///
/// When the `uuid-trigger-ids` feature is enabled, this produces a UUID v7
/// string whose high bits encode a millisecond-precision timestamp, guaranteeing
/// lexicographic (dictionary) ordering that mirrors chronological ordering.
/// This is beneficial for database indexing and ordered event replay.
///
/// This is also called by the public `context::generate_message_id()` API.
pub(crate) fn generate_message_id() -> String {
    #[cfg(feature = "uuid-trigger-ids")]
    {
        uuid::Uuid::now_v7().to_string()
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
    pub trigger_name: &'static str,
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
/// # Elastic Scaling
///
/// The runner uses a [`Semaphore`] to control the number of concurrently
/// executing handler invocations. The event loop does NOT block on handler
/// completion -- each dispatch acquires a semaphore permit and spawns the
/// interceptor chain as an independent `tokio::spawn` task. This allows the
/// event loop to immediately return to `handle_step` for the next event.
///
/// A background `scale_monitor` task periodically observes the semaphore
/// pressure (available permits vs. total permits) and dynamically grows
/// the permit count when the pressure ratio exceeds `policy.scale_threshold`.
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
    name: &'static str,
    /// The `ServiceId` of the trigger service.
    service_id: ServiceId,
    /// Monotonically increasing instance counter for tracing.
    instance_counter: AtomicU64,
    /// The user's event handler.
    handler: TriggerHandler<P>,
    /// Registered interceptor chain (executed in registration order, onion model).
    /// Stored as `Arc` to allow cheap cloning into `tokio::spawn` tasks.
    interceptors: Vec<Arc<dyn TriggerInterceptor<P>>>,
    /// Optional elastic-scaling policy. `None` means serial dispatch
    /// (single permit, no scale monitor).
    scaling: Option<ScalingPolicy>,
    /// Semaphore controlling the number of concurrent handler invocations.
    /// With `scaling = None`, this holds exactly 1 permit (serial mode).
    /// With `scaling = Some(sp)`, starts at `sp.initial_concurrency`.
    semaphore: Arc<Semaphore>,
    /// Current concurrency limit (tracked separately because `Semaphore`
    /// doesn't expose its total permit count).
    current_limit: Arc<AtomicUsize>,
}

impl<P: Send + Sync + 'static> TriggerRunner<P> {
    /// Create a new runner with the given name, handler, restart policy,
    /// and optional scaling policy.
    ///
    /// The built-in `TracingInterceptor` and `RetryInterceptor` are
    /// automatically registered, providing per-dispatch tracing and
    /// exponential-backoff retry for free.
    ///
    /// When `scaling` is `Some`, the semaphore is initialized with
    /// `scaling.initial_concurrency` permits and a background scale
    /// monitor is spawned. When `None`, the semaphore holds exactly
    /// 1 permit (serial dispatch, no elastic scaling).
    ///
    /// The default interceptor order is:
    /// 1. `TracingInterceptor` — wraps everything in a tracing span
    /// 2. `RetryInterceptor` — retries the inner chain on failure
    /// 3. (user interceptors added via `with_interceptor`)
    /// 4. Terminal handler node (implicit)
    pub fn new(
        name: &'static str,
        service_id: ServiceId,
        handler: TriggerHandler<P>,
        restart_policy: RestartPolicy,
        scaling: Option<ScalingPolicy>,
    ) -> Self {
        let initial = scaling.map_or(1, |sp| sp.initial_concurrency);
        Self {
            name,
            service_id,
            instance_counter: AtomicU64::new(0),
            handler,
            interceptors: vec![
                Arc::new(TracingInterceptor),
                Arc::new(RetryInterceptor {
                    policy: restart_policy,
                }),
            ],
            scaling,
            semaphore: Arc::new(Semaphore::new(initial)),
            current_limit: Arc::new(AtomicUsize::new(initial)),
        }
    }

    /// Register an interceptor to the pipeline.
    ///
    /// Interceptors are invoked in registration order (onion model: first
    /// registered = outermost layer). The built-in `TracingInterceptor` and
    /// `RetryInterceptor` are always the first two layers.
    pub fn with_interceptor(mut self, interceptor: Arc<dyn TriggerInterceptor<P>>) -> Self {
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
        // Only spawn the scale monitor if elastic scaling is enabled.
        let monitor_handle = if self.scaling.is_some() {
            Some(self.spawn_scale_monitor())
        } else {
            None
        };

        while !context::is_shutdown() {
            let Some(transition) = Self::poll_next_event(host, &target, self.name).await else {
                break;
            };

            if !self.handle_transition(transition).await {
                break;
            }
        }

        // Stop the scale monitor when the event loop exits
        if let Some(handle) = monitor_handle {
            handle.abort();
        }

        Ok(())
    }

    /// Wait for the next event from the host, racing against the shutdown signal.
    ///
    /// Returns `None` when a shutdown signal is received (caller should break).
    async fn poll_next_event<T, H>(
        host: &mut H,
        target: &Arc<T>,
        name: &str,
    ) -> Option<TriggerTransition<P>>
    where
        T: Send + Sync + 'static,
        H: TriggerHost<T, Payload = P>,
    {
        tokio::select! {
            t = host.handle_step(target) => Some(t),
            _ = context::wait_shutdown() => {
                info!("Trigger '{}' received shutdown, exiting", name);
                None
            }
        }
    }

    /// Dispatch the payload according to the transition type.
    ///
    /// Returns `true` to continue the event loop, `false` to break out.
    async fn handle_transition(&self, transition: TriggerTransition<P>) -> bool {
        match transition {
            TriggerTransition::Next(payload) => {
                self.dispatch(payload).await;
                true
            }
            TriggerTransition::Reload(payload) => {
                self.dispatch(payload).await;
                info!("Trigger '{}' entering reload-wait state", self.name);
                context::wait_shutdown().await;
                false
            }
            TriggerTransition::Stop => {
                info!("Trigger '{}' stopping", self.name);
                false
            }
        }
    }

    // -----------------------------------------------------------------------
    // Elastic scaling -- background pressure monitor
    // -----------------------------------------------------------------------

    /// Spawn a background task that monitors semaphore pressure and
    /// dynamically adjusts concurrency limits.
    ///
    /// # Scaling Algorithm
    ///
    /// The monitor runs on a fixed interval (1 second) and computes:
    ///
    /// ```text
    /// in_flight = current_limit - available_permits
    /// pressure_ratio = in_flight / current_limit
    /// ```
    ///
    /// - **Scale Up**: When `pressure_ratio >= scale_threshold / (scale_threshold + 1)`,
    ///   meaning almost all permits are occupied, the limit is multiplied by
    ///   `scale_factor` (clamped to `max_concurrency`).
    /// - **Scale Down**: When no permits are in use for longer than
    ///   `scale_cooldown`, the limit shrinks back to `initial_concurrency`.
    ///
    /// New permits are added via `Semaphore::add_permits()`; shrinking is
    /// deferred -- we simply stop adding new permits and let the natural
    /// permit release bring the effective concurrency down.
    ///
    /// # Panics
    ///
    /// Panics if called when `self.scaling` is `None`. Callers must check
    /// `self.scaling.is_some()` before calling.
    fn spawn_scale_monitor(&self) -> tokio::task::JoinHandle<()> {
        let semaphore = self.semaphore.clone();
        let current_limit = self.current_limit.clone();
        let scaling = self
            .scaling
            .expect("spawn_scale_monitor requires Some(ScalingPolicy)");
        let trigger_name = self.name;

        tokio::spawn(async move {
            // Track how long the queue has been idle (all permits available)
            let mut idle_since: Option<tokio::time::Instant> = None;

            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;

                let limit = current_limit.load(Ordering::Relaxed);
                let available = semaphore.available_permits();
                let in_flight = limit.saturating_sub(available);

                // --- Path A: Queue is idle (all permits available) ---
                if in_flight == 0 {
                    idle_since.get_or_insert_with(tokio::time::Instant::now);
                    Self::try_scale_down(
                        &semaphore,
                        &current_limit,
                        &scaling,
                        trigger_name,
                        limit,
                        &mut idle_since,
                    );
                    continue;
                }

                // --- Path B: Active handlers present ---
                idle_since = None;
                Self::try_scale_up(
                    &semaphore,
                    &current_limit,
                    &scaling,
                    trigger_name,
                    limit,
                    in_flight,
                );
            }
        })
    }

    /// Attempt to scale down concurrency if the idle cooldown has elapsed.
    ///
    /// Called when `in_flight == 0`. Physically revokes excess permits by
    /// acquiring them via `try_acquire()` and calling `forget()` to permanently
    /// remove them from the semaphore. This ensures `dispatch` truly cannot
    /// exceed the reduced concurrency limit.
    ///
    /// Resets `idle_since` to `None` after successfully scaling down.
    fn try_scale_down(
        semaphore: &Semaphore,
        current_limit: &AtomicUsize,
        scaling: &ScalingPolicy,
        trigger_name: &str,
        limit: usize,
        idle_since: &mut Option<tokio::time::Instant>,
    ) {
        let Some(since) = *idle_since else { return };
        if since.elapsed() < scaling.scale_cooldown || limit <= scaling.initial_concurrency {
            return;
        }

        // Physically revoke excess permits by acquiring and forgetting them.
        // `forget()` permanently reduces the semaphore capacity, ensuring
        // `dispatch` cannot acquire more permits than `initial_concurrency`.
        let to_revoke = limit - scaling.initial_concurrency;
        let mut revoked = 0usize;
        for _ in 0..to_revoke {
            match semaphore.try_acquire() {
                Ok(permit) => {
                    permit.forget();
                    revoked += 1;
                }
                Err(_) => break, // Rare race with new dispatch; stop early
            }
        }

        let new_limit = limit - revoked;
        current_limit.store(new_limit, Ordering::Relaxed);
        info!(
            trigger = %trigger_name,
            old_limit = limit,
            new_limit,
            revoked,
            "Elastic scale-down: revoked {} permits",
            revoked
        );
        *idle_since = None;
    }

    /// Attempt to scale up concurrency if the pressure ratio exceeds the threshold.
    ///
    /// Pressure check: `in_flight >= limit * threshold / (threshold + 1)`.
    /// At default `threshold=5`, this fires at ~83% utilization.
    fn try_scale_up(
        semaphore: &Semaphore,
        current_limit: &AtomicUsize,
        scaling: &ScalingPolicy,
        trigger_name: &str,
        limit: usize,
        in_flight: usize,
    ) {
        if limit >= scaling.max_concurrency {
            return;
        }

        let threshold = scaling.scale_threshold;
        let pressure_limit = limit * threshold / (threshold + 1);

        if in_flight < pressure_limit {
            return;
        }

        let new_limit = (limit * scaling.scale_factor).min(scaling.max_concurrency);
        let added = new_limit - limit;

        if added == 0 {
            return;
        }

        semaphore.add_permits(added);
        current_limit.store(new_limit, Ordering::Relaxed);
        info!(
            trigger = %trigger_name,
            old_limit = limit,
            new_limit,
            in_flight,
            "Elastic scale-up: added {} permits",
            added
        );
    }

    // -----------------------------------------------------------------------
    // Dispatch pipeline
    // -----------------------------------------------------------------------

    /// Dispatch a single event through the interceptor chain asynchronously.
    ///
    /// Acquires a semaphore permit, then spawns the interceptor chain as an
    /// independent tokio task. The event loop does NOT block on completion,
    /// allowing it to immediately process the next event from `handle_step`.
    ///
    /// If the semaphore has no available permits, the event loop will wait
    /// here until a running handler finishes, providing natural backpressure.
    async fn dispatch(&self, payload: P) {
        let seq = self.instance_counter.fetch_add(1, Ordering::Relaxed);
        let message_id = generate_message_id();

        // Acquire a concurrency permit (backpressure point)
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("Semaphore should never be closed");

        let ctx = DispatchContext {
            service_id: self.service_id,
            instance_seq: seq,
            message_id,
            trigger_name: self.name,
            payload: Arc::new(payload),
            handler: self.handler.clone(),
        };

        // Build the interceptor chain (must happen on the current task
        // because interceptors are borrowed from &self)
        let chain = self.build_chain();
        let trigger_name = self.name;

        // Spawn the dispatch as an independent task so the event loop
        // can immediately return to handle_step for the next event.
        tokio::spawn(async move {
            let result = chain(ctx).await;

            if let Err(e) = result {
                warn!(
                    trigger = %trigger_name,
                    error = %e,
                    "Dispatch chain completed with error"
                );
            }

            // Permit is dropped here, releasing the semaphore slot
            drop(permit);
        });
    }

    // -----------------------------------------------------------------------
    // Dispatch pipeline
    // -----------------------------------------------------------------------

    /// Build the interceptor call chain as a `'static` boxed closure.
    ///
    /// Each interceptor `Arc` is cloned so the resulting closure owns all
    /// references and can be safely moved into `tokio::spawn`.
    fn build_chain(
        &self,
    ) -> Box<dyn FnOnce(DispatchContext<P>) -> BoxFuture<'static, anyhow::Result<()>> + Send> {
        // Terminal node: convert DispatchContext -> TriggerContext and call handler
        let terminal: Box<
            dyn FnOnce(DispatchContext<P>) -> BoxFuture<'static, anyhow::Result<()>> + Send,
        > = Box::new(|ctx: DispatchContext<P>| {
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
            }) as BoxFuture<'static, anyhow::Result<()>>
        });

        // Clone Arc references so the chain is 'static and Send
        let interceptor_arcs: Vec<Arc<dyn TriggerInterceptor<P>>> = self.interceptors.to_vec();

        // Fold from back to front, wrapping each interceptor around the previous
        interceptor_arcs
            .into_iter()
            .rev()
            .fold(terminal, |next, interceptor| {
                Box::new(move |ctx| {
                    Box::pin(async move {
                        let next_fn: Next<'_, P> = Box::new(|ctx| next(ctx));
                        interceptor.intercept(ctx, next_fn).await
                    }) as BoxFuture<'static, anyhow::Result<()>>
                })
            })
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
// Retry helper -- shared failure recording logic
// ---------------------------------------------------------------------------

/// Log the failure, record it in the backoff controller, and wait for the
/// computed delay. Returns `Err` if shutdown interrupts the wait, allowing
/// the caller to propagate the original error upward.
async fn record_retry_failure(
    backoff: &mut BackoffController,
    error: anyhow::Error,
) -> anyhow::Result<()> {
    warn!(
        attempt = backoff.attempt_count() + 1,
        error = %error,
        "Trigger handler failed, scheduling retry"
    );
    backoff.record_failure();

    if context::is_shutdown() {
        warn!("Trigger handler retry aborted due to shutdown");
        return Err(error);
    }
    if !context::sleep(backoff.current_delay()).await {
        warn!("Trigger handler retry interrupted by shutdown");
        return Err(error);
    }
    Ok(())
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
/// - Uses [`BackoffController`] with the policy from the [`TriggerRunner`].
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
pub(crate) struct RetryInterceptor {
    /// The backoff policy used for computing retry delays.
    policy: RestartPolicy,
}

impl<P: Send + Sync + 'static> TriggerInterceptor<P> for RetryInterceptor {
    fn intercept<'a>(
        &'a self,
        ctx: DispatchContext<P>,
        next: Next<'a, P>,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let mut backoff = BackoffController::new(self.policy);
            let trigger_max_retries = self.policy.trigger_max_retries;

            // Preserve shared fields for reconstruction across retries
            let service_id = ctx.service_id;
            let instance_seq = ctx.instance_seq;
            let message_id = ctx.message_id;
            let trigger_name = ctx.trigger_name;
            let payload = ctx.payload;
            let handler = ctx.handler;

            // First attempt: use the original `next` closure (enters the
            // interceptor chain below us)
            let first_ctx = DispatchContext {
                service_id,
                instance_seq,
                message_id: message_id.clone(),
                trigger_name,
                payload: payload.clone(),
                handler: handler.clone(),
            };

            if let Err(e) = next(first_ctx).await {
                record_retry_failure(&mut backoff, e).await?;
            } else {
                return Ok(());
            }

            // Subsequent retries: call the handler directly (no need to
            // re-enter interceptors below us, since retry IS the re-entry)
            loop {
                // Safety valve: stop retrying if the trigger_max_retries limit
                // is reached. When trigger_max_retries is None (the designed
                // default), this check is skipped and retries continue
                // indefinitely. This limit does NOT apply to services, which
                // always retry forever.
                if let Some(max) = trigger_max_retries
                    && backoff.attempt_count() >= max
                {
                    warn!(
                        trigger = %trigger_name,
                        attempts = backoff.attempt_count(),
                        trigger_max_retries = max,
                        "Trigger handler exceeded max retry limit, giving up"
                    );
                    return Err(anyhow::anyhow!(
                        "Trigger '{}' exceeded max retry limit ({} attempts)",
                        trigger_name,
                        max
                    ));
                }

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

                if let Err(e) = (handler.clone())(retry_ctx).await {
                    record_retry_failure(&mut backoff, e).await?;
                } else {
                    return Ok(());
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Unit Tests -- Elastic Scaling
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Verifies that the semaphore limits concurrent handler invocations
    /// to the configured `initial_concurrency`.
    #[tokio::test]
    async fn test_semaphore_limits_concurrency() {
        let scaling = ScalingPolicy {
            initial_concurrency: 2,
            max_concurrency: 8,
            ..ScalingPolicy::default()
        };

        let semaphore = Arc::new(Semaphore::new(scaling.initial_concurrency));
        let current_limit = Arc::new(AtomicUsize::new(scaling.initial_concurrency));

        // Acquire 2 permits -- should succeed (matches initial_concurrency)
        let _p1 = semaphore.clone().acquire_owned().await.unwrap();
        let _p2 = semaphore.clone().acquire_owned().await.unwrap();

        // Third acquire should not be immediately available
        let try_result = semaphore.clone().try_acquire_owned();
        assert!(
            try_result.is_err(),
            "Semaphore should be exhausted at initial_concurrency=2"
        );

        assert_eq!(current_limit.load(Ordering::Relaxed), 2);
    }

    /// Verifies that `add_permits` correctly expands the concurrency limit.
    #[tokio::test]
    async fn test_semaphore_scale_up() {
        let scaling = ScalingPolicy {
            initial_concurrency: 1,
            max_concurrency: 4,
            scale_factor: 2,
            ..ScalingPolicy::default()
        };

        let semaphore = Arc::new(Semaphore::new(scaling.initial_concurrency));
        let current_limit = Arc::new(AtomicUsize::new(scaling.initial_concurrency));

        // Simulate scale-up: double the limit
        let limit = current_limit.load(Ordering::Relaxed);
        let new_limit = (limit * scaling.scale_factor).min(scaling.max_concurrency);
        let added = new_limit - limit;

        semaphore.add_permits(added);
        current_limit.store(new_limit, Ordering::Relaxed);

        assert_eq!(current_limit.load(Ordering::Relaxed), 2);
        assert_eq!(semaphore.available_permits(), 2); // 1 original + 1 added

        // Scale up again: 2 -> 4
        let limit = current_limit.load(Ordering::Relaxed);
        let new_limit = (limit * scaling.scale_factor).min(scaling.max_concurrency);
        let added = new_limit - limit;

        semaphore.add_permits(added);
        current_limit.store(new_limit, Ordering::Relaxed);

        assert_eq!(current_limit.load(Ordering::Relaxed), 4);
        assert_eq!(semaphore.available_permits(), 4); // 2 previous + 2 added
    }

    /// Verifies that scale-up respects `max_concurrency` ceiling.
    #[tokio::test]
    async fn test_scale_up_respects_max_concurrency() {
        let scaling = ScalingPolicy {
            initial_concurrency: 1,
            max_concurrency: 3,
            scale_factor: 4,
            ..ScalingPolicy::default()
        };

        let semaphore = Arc::new(Semaphore::new(scaling.initial_concurrency));
        let current_limit = Arc::new(AtomicUsize::new(scaling.initial_concurrency));

        // Scale-up: 1 * 4 = 4, but max is 3 -> clamp to 3
        let limit = current_limit.load(Ordering::Relaxed);
        let new_limit = (limit * scaling.scale_factor).min(scaling.max_concurrency);
        let added = new_limit - limit;

        semaphore.add_permits(added);
        current_limit.store(new_limit, Ordering::Relaxed);

        assert_eq!(current_limit.load(Ordering::Relaxed), 3);
        assert_eq!(semaphore.available_permits(), 3);
    }

    /// Verifies that scale-down physically revokes permits from the semaphore
    /// via `try_acquire()` + `forget()`, not just a logical counter update.
    #[tokio::test]
    async fn test_scale_down_physically_revokes_permits() {
        let scaling = ScalingPolicy {
            initial_concurrency: 1,
            max_concurrency: 8,
            scale_cooldown: Duration::from_millis(10),
            ..ScalingPolicy::default()
        };

        // Simulate a scaled-up state: semaphore has 4 permits, limit = 4
        let semaphore = Arc::new(Semaphore::new(4));
        let current_limit = Arc::new(AtomicUsize::new(4));

        // Wait for cooldown to elapse
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut idle_since = Some(tokio::time::Instant::now() - Duration::from_millis(50));

        TriggerRunner::<()>::try_scale_down(
            &semaphore,
            &current_limit,
            &scaling,
            "test_trigger",
            4, // current limit
            &mut idle_since,
        );

        // Logical limit should be back to initial
        assert_eq!(current_limit.load(Ordering::Relaxed), 1);
        // Physical permits should also be reduced (4 - 3 revoked = 1)
        assert_eq!(
            semaphore.available_permits(),
            1,
            "Semaphore should have physically lost permits after scale-down"
        );
    }

    /// Verifies that a full scale-down → scale-up roundtrip works correctly:
    /// permits are physically revoked during scale-down and physically restored
    /// during scale-up.
    #[tokio::test]
    async fn test_scale_down_then_scale_up_roundtrip() {
        let scaling = ScalingPolicy {
            initial_concurrency: 2,
            max_concurrency: 8,
            scale_factor: 2,
            scale_cooldown: Duration::from_millis(10),
            ..ScalingPolicy::default()
        };

        // Start with a scaled-up state: 4 permits
        let semaphore = Arc::new(Semaphore::new(4));
        let current_limit = Arc::new(AtomicUsize::new(4));

        // --- Phase 1: Scale down from 4 → 2 ---
        tokio::time::sleep(Duration::from_millis(20)).await;
        let mut idle_since = Some(tokio::time::Instant::now() - Duration::from_millis(50));

        TriggerRunner::<()>::try_scale_down(
            &semaphore,
            &current_limit,
            &scaling,
            "test_trigger",
            4,
            &mut idle_since,
        );

        assert_eq!(current_limit.load(Ordering::Relaxed), 2);
        assert_eq!(semaphore.available_permits(), 2);

        // --- Phase 2: Scale up from 2 → 4 ---
        // Simulate pressure: acquire both permits so in_flight = 2
        let _p1 = semaphore.clone().try_acquire_owned().unwrap();
        let _p2 = semaphore.clone().try_acquire_owned().unwrap();

        TriggerRunner::<()>::try_scale_up(
            &semaphore,
            &current_limit,
            &scaling,
            "test_trigger",
            2, // current limit
            2, // in_flight (100% pressure)
        );

        assert_eq!(current_limit.load(Ordering::Relaxed), 4);
        // 2 new permits added, but 2 are held by _p1/_p2
        assert_eq!(semaphore.available_permits(), 2);
    }

    /// Verifies that the pressure calculation correctly identifies when
    /// scaling is needed.
    #[test]
    fn test_pressure_calculation() {
        // With threshold=5, pressure_limit = limit * 5 / 6
        let threshold: usize = 5;

        // Case 1: limit=1 → pressure_limit = 0 → any in_flight triggers scale
        let limit: usize = 1;
        let pressure_limit = limit * threshold / (threshold + 1);
        assert_eq!(pressure_limit, 0);
        assert!(
            1 >= pressure_limit,
            "Single in-flight should trigger scale-up"
        );

        // Case 2: limit=6 → pressure_limit = 5 → need 5+ in_flight to trigger
        let limit: usize = 6;
        let pressure_limit = limit * threshold / (threshold + 1);
        assert_eq!(pressure_limit, 5);
        assert!(5 >= pressure_limit, "5 of 6 should trigger scale-up");
        assert!(4 < pressure_limit, "4 of 6 should NOT trigger scale-up");

        // Case 3: limit=12 → pressure_limit = 10
        let limit: usize = 12;
        let pressure_limit = limit * threshold / (threshold + 1);
        assert_eq!(pressure_limit, 10);
    }

    // -----------------------------------------------------------------------
    // New tests: TriggerRunner conditional scaling initialization
    // -----------------------------------------------------------------------

    /// When `scaling = None`, TriggerRunner should initialize with exactly
    /// 1 permit (serial dispatch) and current_limit = 1.
    #[test]
    fn test_runner_no_scaling_serial_dispatch() {
        let handler: TriggerHandler<String> = Arc::new(|_ctx| Box::pin(async { Ok(()) }));
        let runner = TriggerRunner::new(
            "test_no_scaling",
            ServiceId::new(99),
            handler,
            RestartPolicy::default(),
            None, // no scaling
        );

        // Serial mode: exactly 1 permit available
        assert_eq!(runner.semaphore.available_permits(), 1);
        assert_eq!(runner.current_limit.load(Ordering::Relaxed), 1);
        assert!(runner.scaling.is_none());
    }

    /// When `scaling = Some(ScalingPolicy)`, TriggerRunner should initialize
    /// with `initial_concurrency` permits and store the policy.
    #[test]
    fn test_runner_with_scaling_initializes_permits() {
        let sp = ScalingPolicy {
            initial_concurrency: 4,
            max_concurrency: 16,
            ..ScalingPolicy::default()
        };
        let handler: TriggerHandler<String> = Arc::new(|_ctx| Box::pin(async { Ok(()) }));
        let runner = TriggerRunner::new(
            "test_with_scaling",
            ServiceId::new(100),
            handler,
            RestartPolicy::default(),
            Some(sp),
        );

        assert_eq!(runner.semaphore.available_permits(), 4);
        assert_eq!(runner.current_limit.load(Ordering::Relaxed), 4);
        assert!(runner.scaling.is_some());
        let stored = runner.scaling.unwrap();
        assert_eq!(stored.max_concurrency, 16);
    }

    /// Verify that the default ScalingPolicy (used by TopicHost) produces
    /// expected initial values in TriggerRunner.
    #[test]
    fn test_runner_default_scaling_policy_values() {
        let sp = ScalingPolicy::default();
        let handler: TriggerHandler<String> = Arc::new(|_ctx| Box::pin(async { Ok(()) }));
        let runner = TriggerRunner::new(
            "test_default_sp",
            ServiceId::new(101),
            handler,
            RestartPolicy::default(),
            Some(sp),
        );

        // Default initial_concurrency is 1
        assert_eq!(runner.semaphore.available_permits(), 1);
        assert_eq!(runner.current_limit.load(Ordering::Relaxed), 1);
        // But scaling IS enabled
        assert!(runner.scaling.is_some());
        assert_eq!(runner.scaling.unwrap().max_concurrency, 64);
    }

    /// Verify that custom ScalingPolicy via builder integrates correctly
    /// with TriggerRunner initialization.
    #[test]
    fn test_runner_custom_scaling_via_builder() {
        let sp = ScalingPolicy::builder()
            .initial_concurrency(8)
            .max_concurrency(64)
            .scale_factor(4)
            .scale_threshold(3)
            .scale_cooldown(Duration::from_secs(10))
            .build();

        let handler: TriggerHandler<String> = Arc::new(|_ctx| Box::pin(async { Ok(()) }));
        let runner = TriggerRunner::new(
            "test_builder_sp",
            ServiceId::new(102),
            handler,
            RestartPolicy::default(),
            Some(sp),
        );

        assert_eq!(runner.semaphore.available_permits(), 8);
        assert_eq!(runner.current_limit.load(Ordering::Relaxed), 8);
        let stored = runner.scaling.unwrap();
        assert_eq!(stored.scale_factor, 4);
        assert_eq!(stored.scale_threshold, 3);
        assert_eq!(stored.scale_cooldown, Duration::from_secs(10));
    }

    // -----------------------------------------------------------------------
    // UUID v7 monotonic ordering test
    // -----------------------------------------------------------------------

    /// Verifies that `generate_message_id` produces lexicographically ordered
    /// IDs when the `uuid-trigger-ids` feature is enabled (UUID v7).
    ///
    /// UUID v7 encodes a millisecond-precision timestamp in its high bits,
    /// so consecutive calls should yield IDs that sort in chronological order.
    /// This property is critical for database index locality and ordered
    /// event replay.
    #[cfg(feature = "uuid-trigger-ids")]
    #[test]
    fn test_message_id_monotonic_ordering() {
        let mut previous = generate_message_id();
        for _ in 0..100 {
            let current = generate_message_id();
            assert!(
                current >= previous,
                "UUID v7 IDs must be monotonically non-decreasing: \
                 previous={previous}, current={current}"
            );
            previous = current;
        }
    }
}
