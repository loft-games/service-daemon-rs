use anyhow::Result;
use chrono::Utc;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::task::{JoinError, JoinHandle};
use tracing::warn;

use crate::core::context;
use crate::core::provider_init::{ProviderRuntimePhase, with_provider_runtime_phase};
use crate::core::runtime_facts::TriggerRuntimeFactsHandle;
use crate::models::policy::RestartPolicy;
use crate::models::service::ServiceInstanceId;
use crate::models::trigger::{TriggerContext, TriggerHandler, TriggerMessage};
use uuid::Uuid;

use super::TriggerRunner;
use super::failure::{TriggerDispatchFailure, TriggerDispatchFailureKind};
use super::message_id::generate_message_id;

pub(super) type DispatchTaskOutcome = std::result::Result<(), TriggerDispatchFailure>;
pub(super) type InFlightDispatches = FuturesUnordered<BoxFuture<'static, DispatchTaskOutcome>>;

pub(super) struct AbortOnDropJoinHandle<T> {
    handle: JoinHandle<T>,
}

impl<T> AbortOnDropJoinHandle<T> {
    pub(super) fn new(handle: JoinHandle<T>) -> Self {
        Self { handle }
    }

    pub(super) async fn join(&mut self) -> std::result::Result<T, JoinError> {
        (&mut self.handle).await
    }
}

impl<T> Drop for AbortOnDropJoinHandle<T> {
    fn drop(&mut self) {
        self.handle.abort();
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
    /// The `ServiceInstanceId` of the trigger service.
    pub service_instance_id: ServiceInstanceId,
    /// The `ServiceInstanceId` of the service that originally emitted the event.
    pub source_service_instance_id: ServiceInstanceId,
    /// Monotonically increasing sequence number within this trigger service.
    pub instance_seq: u64,
    /// Service generation that owns this dispatch.
    pub generation: u64,
    /// Globally unique identifier for this event instance (UUID v7, time-ordered).
    pub message_id: uuid::Uuid,
    /// Human-readable name of this trigger service (for logging/tracing).
    pub trigger_name: &'static str,
    /// The business payload, wrapped in `Arc` for cheap cloning across retries.
    pub payload: Arc<P>,
    /// The user's event handler (needed at the terminal node of the chain).
    pub handler: TriggerHandler<P>,
    /// Read-only runtime facts writer for this trigger dispatch.
    pub runtime_facts: Option<TriggerRuntimeFactsHandle>,
    /// Retry/backoff policy captured at this dispatch boundary.
    pub retry_policy: RestartPolicy,
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
/// interceptor chain is dynamically composable via `Vec<Arc<dyn ...>>`.
///
/// # Internal Generic Interceptor
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

impl<P: Send + Sync + 'static> TriggerRunner<P> {
    /// Dispatch a single event through the interceptor chain asynchronously.
    ///
    /// Acquires a semaphore permit, then spawns the interceptor chain as an
    /// independent tokio task. The event loop does NOT block on completion,
    /// allowing it to immediately process the next event from `handle_step`.
    ///
    /// If the semaphore has no available permits, the event loop will wait
    /// here until a running handler finishes, providing natural backpressure.
    pub(super) async fn dispatch(
        &self,
        payload: P,
        identity: Option<(uuid::Uuid, ServiceInstanceId)>,
        in_flight: &mut InFlightDispatches,
    ) -> Result<()> {
        let seq = self.instance_counter.fetch_add(1, Ordering::Relaxed);
        let (message_id, source_service_instance_id) =
            identity.unwrap_or_else(|| (generate_message_id(), self.service_instance_id));
        if let Some(policy_overlays) = &self.policy_overlays {
            policy_overlays.apply_effective_concurrency(self.service_instance_id, self.generation);
        }

        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| {
                TriggerDispatchFailure::new(
                    TriggerDispatchFailureKind::DispatchPermitAcquireFailed,
                    self.name,
                    self.service_instance_id,
                    Some(seq),
                    Some(message_id),
                    format!("could not acquire dispatch permit: {error}"),
                )
            })?;
        if let Some(runtime_facts) = &self.runtime_facts {
            runtime_facts.record_dispatched();
        }
        let effective_policy = self
            .policy_overlays
            .as_ref()
            .map(|store| {
                context::effective_trigger_policy(
                    store,
                    self.service_instance_id,
                    self.generation,
                    self.base_policy,
                )
            })
            .unwrap_or_else(
                || crate::core::trigger_policy_overlay::EffectiveTriggerPolicy {
                    restart_policy: self.base_policy.restart_policy,
                    dispatch_timeout: None,
                },
            );

        let ctx = DispatchContext {
            service_instance_id: self.service_instance_id,
            source_service_instance_id,
            instance_seq: seq,
            generation: self.generation,
            message_id,
            trigger_name: self.name,
            payload: Arc::new(payload),
            handler: self.handler.clone(),
            runtime_facts: self.runtime_facts.clone(),
            retry_policy: effective_policy.restart_policy,
        };

        let chain = self.build_chain();
        let trigger_name = self.name;
        let service_instance_id = self.service_instance_id;
        let dispatch_timeout = effective_policy.dispatch_timeout;

        let dispatch_task = context::spawn_with_context(async move {
            let dispatch = with_provider_runtime_phase(
                ProviderRuntimePhase::TriggerDispatchResolve,
                chain(ctx),
            );
            let result = run_dispatch_with_timeout(
                dispatch,
                dispatch_timeout,
                trigger_name,
                service_instance_id,
                seq,
                message_id,
            )
            .await;
            drop(permit);
            result
        });

        in_flight.push(Self::observe_dispatch_task(
            trigger_name,
            service_instance_id,
            seq,
            message_id,
            self.runtime_facts.clone(),
            dispatch_task,
        ));

        Ok(())
    }

    pub(super) fn observe_dispatch_task(
        trigger_name: &'static str,
        service_instance_id: ServiceInstanceId,
        instance_seq: u64,
        message_id: Uuid,
        runtime_facts: Option<TriggerRuntimeFactsHandle>,
        handle: JoinHandle<anyhow::Result<()>>,
    ) -> BoxFuture<'static, DispatchTaskOutcome> {
        Box::pin(async move {
            let mut handle = AbortOnDropJoinHandle::new(handle);
            match handle.join().await {
                Ok(Ok(())) => {
                    if let Some(runtime_facts) = &runtime_facts {
                        runtime_facts.record_completed();
                    }
                    Ok(())
                }
                Ok(Err(error)) => match error.downcast::<TriggerDispatchFailure>() {
                    Ok(failure) => {
                        if let Some(runtime_facts) = &runtime_facts {
                            runtime_facts.record_failed(failure.to_string());
                        }
                        warn!(
                            trigger = %failure.trigger_name(),
                            service_instance_id = %failure.service_instance_id(),
                            instance_seq = ?failure.instance_seq(),
                            message_id = ?failure.message_id(),
                            trigger_failure_kind = %failure.kind().as_str(),
                            error = %failure,
                            "Dispatch chain completed with typed failure"
                        );
                        Err(failure)
                    }
                    Err(error) => {
                        let error_message = error.to_string();
                        let failure = TriggerDispatchFailure::new(
                            TriggerDispatchFailureKind::DispatchTaskError,
                            trigger_name,
                            service_instance_id,
                            Some(instance_seq),
                            Some(message_id),
                            error_message,
                        );
                        if let Some(runtime_facts) = &runtime_facts {
                            runtime_facts.record_failed(failure.to_string());
                        }
                        warn!(
                            trigger = %trigger_name,
                            service_instance_id = %service_instance_id,
                            instance_seq,
                            message_id = %message_id,
                            trigger_failure_kind = %failure.kind().as_str(),
                            error = %failure,
                            "Dispatch chain completed with error"
                        );
                        Err(failure)
                    }
                },
                Err(join_error) => {
                    let kind = if join_error.is_panic() {
                        TriggerDispatchFailureKind::DispatchTaskPanic
                    } else {
                        TriggerDispatchFailureKind::DispatchTaskCancelled
                    };
                    let failure = TriggerDispatchFailure::new(
                        kind,
                        trigger_name,
                        service_instance_id,
                        Some(instance_seq),
                        Some(message_id),
                        join_error.to_string(),
                    );
                    if let Some(runtime_facts) = &runtime_facts {
                        runtime_facts.record_failed(failure.to_string());
                    }
                    warn!(
                        trigger = %trigger_name,
                        service_instance_id = %service_instance_id,
                        instance_seq,
                        message_id = %message_id,
                        trigger_failure_kind = %failure.kind().as_str(),
                        error = %failure,
                        "Dispatch task did not complete normally"
                    );
                    Err(failure)
                }
            }
        })
    }

    // -----------------------------------------------------------------------
    // Dispatch pipeline
    // -----------------------------------------------------------------------

    /// Build the interceptor call chain as a `'static` boxed closure.
    ///
    /// Each interceptor `Arc` is cloned so the resulting closure owns all
    /// references and can be safely moved into `tokio::spawn`.
    pub(super) fn build_chain(
        &self,
    ) -> Box<dyn FnOnce(DispatchContext<P>) -> BoxFuture<'static, anyhow::Result<()>> + Send> {
        // Terminal node: convert DispatchContext -> TriggerContext and call handler
        let terminal: Box<
            dyn FnOnce(DispatchContext<P>) -> BoxFuture<'static, anyhow::Result<()>> + Send,
        > = Box::new(|ctx: DispatchContext<P>| {
            Box::pin(async move {
                let trigger_ctx = TriggerContext::new(
                    ctx.service_instance_id,
                    ctx.generation,
                    ctx.instance_seq,
                    TriggerMessage {
                        message_id: ctx.message_id,
                        source_service_instance_id: ctx.source_service_instance_id,
                        timestamp: Utc::now(),
                        payload: ctx.payload,
                    },
                );
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

async fn run_dispatch_with_timeout(
    dispatch: impl std::future::Future<Output = anyhow::Result<()>>,
    timeout: Option<Duration>,
    trigger_name: &'static str,
    service_instance_id: ServiceInstanceId,
    instance_seq: u64,
    message_id: Uuid,
) -> anyhow::Result<()> {
    let Some(timeout) = timeout else {
        return dispatch.await;
    };
    match tokio::time::timeout(timeout, dispatch).await {
        Ok(result) => result,
        Err(_) => Err(TriggerDispatchFailure::new(
            TriggerDispatchFailureKind::DispatchTimedOut,
            trigger_name,
            service_instance_id,
            Some(instance_seq),
            Some(message_id),
            format!("dispatch did not complete within {timeout:?}"),
        )
        .into()),
    }
}
