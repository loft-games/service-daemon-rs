# Future Work: Interceptor Middleware Pattern (Plan C)

> **Status**: Deferred — needs full feasibility study  
> **Created**: 2026-02-27  
> **Context**: During the trigger middleware refactoring, we identified an
> ideal-state architecture that would fully decouple retry, tracing, and
> other cross-cutting concerns into composable middleware. However, Rust's
> type system constraints (generic `P` in trait objects) make a naive
> implementation impossible. This document captures the design direction
> for future evaluation.

## Current State (After Plan B)

```
dispatch_with_middleware(payload)
  ├── middleware.before_dispatch()        ← observer, can't control flow
  ├── invoke_handler_with_retry(payload)  ← retry is a Runner private method
  └── middleware.after_dispatch()          ← observer, can't control flow
```

Retry and tracing are **hardcoded inside the runner**: flexible enough for
now, but not user-extensible.

## Ideal State (Plan C)

```
dispatch(payload)
  └── TracingMiddleware.call(ctx, next)        ← wraps everything in a span
        └── RetryMiddleware.call(ctx, next)    ← calls next N times on failure
              └── handler(ctx)                 ← final handler
```

Each middleware owns its `next` reference and decides **if, when, and how
many times** to call it (Tower `Service` model).

## Key Blocker: Generic `P` in Trait Objects

```rust
// This CANNOT work as a trait object:
trait TriggerMiddleware {
    fn call<P>(&self, ctx: &mut DispatchContext<P>, next: ...) -> Result<()>;
    //      ^^ generic methods are not object-safe
}

// Option A: Lift P to trait level → middleware not reusable across types
trait TriggerMiddleware<P> { ... }

// Option B: Type-erase P via Arc<dyn Any> → loses compile-time safety
struct DispatchContext { payload: Arc<dyn Any + Send + Sync> }

// Option C: Static dispatch via generics → no Vec<Box<dyn Middleware>>
// (Tower's approach, but requires complex type-level recursion)
```

This is the **same fundamental tension** as the TopicHost generic problem.

## Evaluation Criteria for Future Revisit

1. Can we accept `TriggerMiddleware<P>` (non-reusable across payload types)?
   - If middleware is mostly framework-internal, this may be acceptable.
2. Would a proc-macro (`#[trigger(middlewares = [Retry, Tracing])]`) generate
   the static dispatch chain at compile time, avoiding trait objects entirely?
3. Are there new Rust features (e.g., `async fn in trait`, GATs) that could
   simplify the lifetime/generics issues?

## References

- [Tower Service trait](https://docs.rs/tower/latest/tower/trait.Service.html)
- [Axum middleware pattern](https://docs.rs/axum/latest/axum/middleware/index.html)
