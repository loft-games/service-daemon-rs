---
name: sd-service-author
description: "[user] Author service-daemon-rs #[service] functions. Use when writing or reviewing a #[service] for the service-daemon Rust framework: the function signature, dependency injection as Arc<T>, the shutdown-aware loop, readiness handshake (done()), and fatal-vs-recoverable error semantics."
---

# Authoring `#[service]` for service-daemon-rs

A `#[service]` is a long-running async task the daemon supervises: it resolves the
function's dependencies, places it on a runtime, starts it in a priority wave,
restarts it on recoverable failure, and signals shutdown.

This SKILL.md is the entry point. Load the companions for depth:

- `reference.md` — signature rules, lifecycle helpers, error/restart semantics,
  scheduling modes, and the readiness handshake.
- `pitfalls.md` — the mistakes that compile but misbehave.
- `examples/` — copy-paste service templates.

## Shape

```rust
use std::sync::Arc;
use std::time::Duration;
use service_daemon::service;

#[service]
pub async fn heartbeat(cfg: Arc<AppConfig>) -> anyhow::Result<()> {
    while !service_daemon::is_shutdown() {
        // `sleep` is interruptible: returns false when shutdown arrives.
        if !service_daemon::sleep(Duration::from_secs(5)).await {
            break;
        }
    }
    Ok(())
}
```

- Dependencies are injected as `Arc<T>` (every `#[provider]` type). The macro
  resolves them; you never construct them.
- Prefer `async fn`. A synchronous service requires `#[allow(sync_handler)]` or it
emits a runtime warning.
- Register happens automatically (link-time); no manual registration.

## The rules that matter most

1. **Be shutdown-aware.** Loop on `while !service_daemon::is_shutdown()` and use
   `service_daemon::sleep(..)` (returns `false` when shutdown arrives) instead of
   `tokio::time::sleep`, so the service exits promptly.
2. **Signal readiness** so dependents in later waves can start. Call
   `service_daemon::done()` once init is complete, or rely on the implicit
   handshake (the first `is_shutdown()`/`sleep()` call marks the service healthy).
3. **Classify failure.** Returning `ServiceError::Fatal` terminates the service
   with no restart. An ordinary `Err(..)` or panic restarts it with backoff. A
   clean `Ok(())` starts a fresh generation immediately (not counted as failure).
4. **Set `priority`** to order startup waves (high→low) and shutdown (low→high).

See `reference.md` for the full lifecycle and error model, and the
`sd-daemon-bootstrap` skill (if installed) for wiring services into a daemon.
