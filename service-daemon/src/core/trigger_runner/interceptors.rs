use anyhow::Error;
use chrono::Utc;
use futures::future::BoxFuture;
use tracing::{Instrument, info, warn};

use crate::core::context;
use crate::core::runtime_facts::TriggerRuntimeFactsHandle;
use crate::models::policy::BackoffController;
use crate::models::trigger::{TriggerContext, TriggerMessage};

use super::dispatch::{DispatchContext, Next, TriggerInterceptor};
use super::failure::{TriggerDispatchFailure, TriggerDispatchFailureKind};

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
/// - `source_service_instance_id`: Originating `ServiceInstanceId` value.
/// - `instance_seq`: Monotonic sequence number within the service.
/// - `message_id`: The globally unique event identifier.
///
/// # Log Output
///
/// ```text
/// INFO trigger{name="my_trigger" service_instance_id=svcinst#... message_id="msg-0"}: Trigger fired
/// ```
pub(super) struct TracingInterceptor;

impl<P: Send + Sync + 'static> TriggerInterceptor<P> for TracingInterceptor {
    fn intercept<'a>(
        &'a self,
        ctx: DispatchContext<P>,
        next: Next<'a, P>,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let mid_val = ctx.message_id.as_u128();
            let span = tracing::info_span!(
                "trigger",
                name = %ctx.trigger_name,
                service_instance_id = %ctx.service_instance_id,
                source_service_instance_id = %ctx.source_service_instance_id,
                instance_seq = ctx.instance_seq,
                message_id = %ctx.message_id,
                mid_hi = (mid_val >> 64) as u64,
                mid_lo = mid_val as u64,
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
/// computed delay. Returns `false` when reload or shutdown interrupts the wait.
async fn record_retry_failure(
    backoff: &mut BackoffController,
    error: Error,
    runtime_facts: Option<&TriggerRuntimeFactsHandle>,
) -> bool {
    let error_message = error.to_string();
    if let Some(runtime_facts) = runtime_facts {
        runtime_facts.record_retry(error_message.clone());
    }
    warn!(
        attempt = backoff.attempt_count() + 1,
        error = %error,
        "Trigger handler failed, scheduling retry"
    );
    backoff.record_failure();

    if context::is_shutdown() {
        warn!(error = %error, "Trigger handler retry aborted due to shutdown or reload");
        return false;
    }
    if !context::sleep(backoff.current_delay()).await {
        warn!(error = %error, "Trigger handler retry interrupted by shutdown or reload");
        return false;
    }
    true
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
/// - Uses [`BackoffController`] with the dispatch-captured retry policy.
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
pub(super) struct RetryInterceptor;

impl<P: Send + Sync + 'static> TriggerInterceptor<P> for RetryInterceptor {
    fn intercept<'a>(
        &'a self,
        ctx: DispatchContext<P>,
        next: Next<'a, P>,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let retry_policy = ctx.retry_policy;
            let mut backoff = BackoffController::new(retry_policy);
            let trigger_max_retries = retry_policy.trigger_max_retries;

            // Preserve shared fields for reconstruction across retries
            let service_instance_id = ctx.service_instance_id;
            let source_service_instance_id = ctx.source_service_instance_id;
            let instance_seq = ctx.instance_seq;
            let generation = ctx.generation;
            let message_id = ctx.message_id;
            let trigger_name = ctx.trigger_name;
            let payload = ctx.payload;
            let handler = ctx.handler;
            let runtime_facts = ctx.runtime_facts;

            // First attempt: use the original `next` closure (enters the
            // interceptor chain below us)
            let first_ctx = DispatchContext {
                service_instance_id,
                source_service_instance_id,
                instance_seq,
                generation,
                message_id,
                trigger_name,
                payload: payload.clone(),
                handler: handler.clone(),
                runtime_facts: runtime_facts.clone(),
                retry_policy,
            };

            if let Err(e) = next(first_ctx).await {
                if !record_retry_failure(&mut backoff, e, runtime_facts.as_ref()).await {
                    return Ok(());
                }
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
                    return Err(TriggerDispatchFailure::new(
                        TriggerDispatchFailureKind::HandlerRetryExhausted,
                        trigger_name,
                        service_instance_id,
                        Some(instance_seq),
                        Some(message_id),
                        format!("exceeded max retry limit ({max} attempts)"),
                    )
                    .into());
                }

                let retry_ctx = TriggerContext::new(
                    service_instance_id,
                    generation,
                    instance_seq,
                    TriggerMessage {
                        message_id,
                        source_service_instance_id,
                        timestamp: Utc::now(),
                        payload: payload.clone(),
                    },
                );

                if let Err(e) = (handler.clone())(retry_ctx).await {
                    if !record_retry_failure(&mut backoff, e, runtime_facts.as_ref()).await {
                        return Ok(());
                    }
                } else {
                    return Ok(());
                }
            }
        })
    }
}
