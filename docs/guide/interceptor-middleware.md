# Interceptor Middleware

The trigger dispatch pipeline uses a composable **interceptor chain** (onion model) that gives
each layer full control over when, whether, and how many times the next layer is invoked.

## 1. Architecture

```text
dispatch(payload)
  +-- TracingInterceptor.intercept(ctx, next)       ← outermost: wraps in tracing span
        +-- RetryInterceptor.intercept(ctx, next)   ← retries inner chain on failure
              +-- [user interceptors...]
                    +-- handler(TriggerContext)      ← terminal: calls user handler
```

Each interceptor receives a `DispatchContext<P>` (owned) and a `next` callback. It decides **if, when,
and how many times** to call `next`, enabling patterns like retry, tracing, rate limiting, and circuit breaking.

## 2. Built-in Interceptors

| Interceptor | Position | Responsibility |
|:---|:---|:---|
| `TracingInterceptor` | Outermost | Creates `info_span!("trigger", name, instance_id, message_id)` |
| `RetryInterceptor` | Second | Exponential-backoff retry using the runner's `RestartPolicy` |

Both are automatically registered by `TriggerRunner::new()`.

## 3. Key Types

### `DispatchContext<P>`
The data envelope that flows through the chain:

```rust
pub struct DispatchContext<P> {
    pub service_id: ServiceId,
    pub instance_seq: u64,
    pub message_id: String,
    pub trigger_name: String,
    pub payload: Arc<P>,
    pub handler: TriggerHandler<P>,
}
```

The context is passed **by value** (moved) through the chain to avoid borrow conflicts across nested closures.

### `Next<'a, P>`
A boxed closure representing the rest of the chain:

```rust
type Next<'a, P> = Box<
    dyn FnOnce(DispatchContext<P>) -> BoxFuture<'a, anyhow::Result<()>> + Send + 'a,
>;
```

### `TriggerInterceptor<P>`
The core trait:

```rust
pub trait TriggerInterceptor<P: Send + Sync + 'static>: Send + Sync {
    fn intercept<'a>(
        &'a self,
        ctx: DispatchContext<P>,
        next: Next<'a, P>,
    ) -> BoxFuture<'a, anyhow::Result<()>>;
}
```

## 4. Writing a Custom Interceptor

### Generic Interceptor (any payload)
Use a blanket `impl<P>` — works with every trigger type:

```rust
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

            let result = next(ctx).await;

            tracing::info!(
                elapsed_ms = start.elapsed().as_millis(),
                "Dispatch completed"
            );
            result
        })
    }
}
```

### Payload-Specific Interceptor
Implement only for a concrete payload type:

```rust
impl TriggerInterceptor<SmsPayload> for SmsAuditInterceptor {
    fn intercept<'a>(
        &'a self,
        ctx: DispatchContext<SmsPayload>,
        next: Next<'a, SmsPayload>,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            // Access typed payload directly -- no downcast needed
            tracing::info!(phone = %ctx.payload.phone_number, "SMS dispatch started");
            next(ctx).await
        })
    }
}
```

> [!TIP]
> Payload-specific interceptors can only be registered on `TriggerRunner<SmsPayload>`.
> The compiler enforces this at build time — no runtime surprises.

## 5. Interceptor Patterns

### Pass-Through (observe only)
```rust
let result = next(ctx).await;
result
```

### Short-Circuit (reject)
```rust
if !self.is_allowed(&ctx) {
    return Err(anyhow::anyhow!("Rejected by policy"));
}
next(ctx).await
```

### Retry (call multiple times)
The built-in `RetryInterceptor` demonstrates this pattern — it calls `next` once,
then reconstructs the context and calls the handler directly for subsequent attempts.

### Wrap in Span (tracing)
```rust
let span = tracing::info_span!("my_span");
next(ctx).instrument(span).await
```

## 6. Design: Semi-Static Dispatch

The type parameter `P` is bound at the **trait level** (`TriggerInterceptor<P>`), not at the method level.
This makes the trait object-safe within a specific `TriggerRunner<P>`:

- `Vec<Arc<dyn TriggerInterceptor<P>>>` — dynamic composition with safe cross-task sharing
- Full compile-time payload type safety — no `Any` or `downcast`
- Generic interceptors via `impl<P> TriggerInterceptor<P> for T`
- Payload-specific interceptors via `impl TriggerInterceptor<MyPayload> for T`
- Interceptors are `Arc`-wrapped, enabling safe `tokio::spawn` for async dispatch

---

## 7. More Information

- [Extending the Framework](../development/extending-framework.md#3-adding-custom-interceptors): Quick-start for framework developers.
- [Trigger Guide](triggers.md#5-resilience-automatic-handler-retries): How retry works from the user's perspective.
- [Architecture Overview](../architecture/internal-overview.md#7-event-traceability-architecture): System-level view of the interceptor pipeline.

[Back to README](../../README.md)
