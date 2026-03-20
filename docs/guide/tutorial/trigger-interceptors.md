# Trigger Middlewares (Interceptors)

You've seen how the framework automatically retries failed handlers and wraps every dispatch in a tracing span. But have you ever wondered *where* that implementation lives? And more importantly -- can you add your own logic?

The answer is **interceptors**: composable middleware layers that wrap the trigger dispatch pipeline.

---

## 1. The Onion Model

Every time a trigger fires, the event payload passes through a chain of interceptors before reaching your handler. Each interceptor wraps the next, like layers of an onion:

```text
dispatch(payload)
  +-- TracingInterceptor         <- creates the tracing span
        +-- RetryInterceptor     <- retries on failure
              +-- [your interceptors here]
                    +-- handler  <- your business logic
```

Each layer decides **if, when, and how many times** to call the next one.

## 2. Your First Interceptor

Let's write a simple timing interceptor that logs how long each dispatch takes.

```rust,ignore
use service_daemon::core::trigger_runner::{
    DispatchContext, Next, TriggerInterceptor,
};
use futures::future::BoxFuture;

pub struct TimingInterceptor;

impl<P: Send + Sync + 'static> TriggerInterceptor<P> for TimingInterceptor {
    fn intercept<'a>(
        &'a self,
        ctx: DispatchContext<P>,
        next: Next<'a, P>,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let start = std::time::Instant::now();

            // Call the next layer (continue the chain)
            let result = next(ctx).await;

            let elapsed = start.elapsed();
            tracing::info!(elapsed_ms = elapsed.as_millis(), "Dispatch timing");

            result
        })
    }
}
```

That's it. Three key things happened:
1. **Pre-processing**: We captured the start time.
2. **Delegation**: We called `next(ctx)` to continue the chain.
3. **Post-processing**: We logged the elapsed time.

> [!TIP]
> Notice the `impl<P: Send + Sync + 'static>` -- this makes the interceptor work with **any** payload type. The compiler ensures type safety within each `TriggerRunner<P>` instance.

## 3. The Power of Control

Unlike simple "before/after" hooks, interceptors have full control over the flow. Here are some patterns:

### Short-Circuit (Reject)
```rust,ignore
Box::pin(async move {
    if !self.is_allowed(&ctx.trigger_name) {
        return Err(anyhow::anyhow!("Trigger blocked by policy"));
    }
    next(ctx).await
})
```

### Observe Only (Pass-Through)
```rust,ignore
Box::pin(async move {
    tracing::debug!("About to dispatch: {}", ctx.trigger_name);
    next(ctx).await
})
```

### Wrap in Context (e.g., Tracing Span)
```rust,ignore
Box::pin(async move {
    let span = tracing::info_span!("my_custom_span");
    next(ctx).instrument(span).await
})
```

## 4. Payload-Specific Interceptors

Sometimes you want an interceptor that only works with a specific payload type. No problem -- just implement it for that type only:

```rust,ignore
impl TriggerInterceptor<SmsPayload> for SmsAuditInterceptor {
    fn intercept<'a>(
        &'a self,
        ctx: DispatchContext<SmsPayload>,
        next: Next<'a, SmsPayload>,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            // Direct access to typed fields -- no downcasting!
            tracing::info!(
                phone = %ctx.payload.phone_number,
                "SMS dispatch audit"
            );
            next(ctx).await
        })
    }
}
```

The compiler will enforce that this interceptor can only be registered on a `TriggerRunner<SmsPayload>`. Try to use it with a different payload type? Compilation error. No surprises at runtime.

## 5. Under the Hood: `DispatchContext`

The context that flows through the chain carries everything needed for dispatch:

```rust,ignore
pub struct DispatchContext<P> {
    pub service_id: ServiceId,     // Which trigger service
    pub instance_seq: u64,         // Invocation sequence number
    pub message_id: String,        // Globally unique event ID
    pub trigger_name: &'static str, // Human-readable trigger name
    pub payload: Arc<P>,           // Your business data (cheap to clone)
    pub handler: TriggerHandler<P>, // The final handler function
}
```

The context is passed **by value** -- each interceptor takes ownership, can read or modify fields, then passes it to `next`. This eliminates the borrow-checker headaches that would arise from `&mut` references across nested async closures.

---

> [!NOTE]
> **Why "semi-static dispatch"?** The payload type `P` is fixed per `TriggerRunner<P>`, so you get full compile-time type safety. But the interceptor chain itself is a `Vec<Arc<dyn TriggerInterceptor<P>>>`, giving you runtime flexibility to add or skip interceptors dynamically - and safe cross-task sharing for async dispatch.

---

[**<- Previous Step: Custom Trigger Implementation**](docs/guide/tutorial/tailor-made-triggers.md) | [**Next Step: Advanced Macro Usage ->**](docs/guide/tutorial/advanced-macros.md)
