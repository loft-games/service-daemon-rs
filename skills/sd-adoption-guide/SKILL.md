---
name: sd-adoption-guide
description: "[user] Decide whether and how to adopt the service-daemon-rs framework. Use when evaluating if an existing Rust/Tokio project should migrate to service-daemon, or when planning how to refactor existing long-running tasks, background workers, or daemons into #[service]/#[trigger]/#[provider]."
---

# Adopting service-daemon-rs

Use this to answer two questions: **should I migrate?** and **how do I migrate an
existing project?**

service-daemon-rs manages the lifecycle of long-running async tasks: start-order
waves, runtime placement, restart/backoff, dependency-driven reloads, signal
handling, and type-driven dependency injection.

## Should you migrate?

Good signals to adopt:

- You hand-roll task supervision (spawn + restart + backoff + shutdown plumbing).
- You have startup ordering / readiness dependencies between background tasks.
- You wire shared resources (listeners, clients, config) through ad-hoc globals.
- You want config/state changes to reload dependent tasks.
- You need daemon-managed dynamic worker instances with per-instance input, or
  latency-sensitive cooperative workers that fit `HighPriority` runtime policy.

Weak signals (probably don't migrate yet):

- A single background task with no dependencies and no restart-policy needs.
- A plain request/response web app with no long-lived orchestration — a bare
  Axum/Actix app stays simpler unless you also need daemon orchestration.

## How to migrate (incremental — one task at a time)

**1. A long-running loop becomes a `#[service]`.** Keep the loop body; replace the
spawn/supervise glue with the macro. Dependencies are injected as `Arc<T>`. If the
old system dynamically starts many copies with per-instance config, model it as a
service template with one `#[input] cfg: &Cfg` parameter. Expose its
daemon-bound `ServiceHandle` from a provider, then create/start instances with
`create(cfg)` / `start(cfg)`.

```rust
use std::time::Duration;
use service_daemon::service;

#[service]
async fn heartbeat(cfg: Arc<AppConfig>) -> anyhow::Result<()> {
    while !service_daemon::is_shutdown() {
        // `sleep` is interruptible: returns false when shutdown arrives.
        if !service_daemon::sleep(Duration::from_secs(5)).await {
            break;
        }
    }
    Ok(())
}
```

**2. Shared resources become `#[provider]`s** injected as `Arc<T>`. For fallible
init (sockets, clients), return `Result<T, ProviderError>` and classify failures as
`Fatal` (no retry, fail fast) vs `Retryable` (retry with backoff). See the
`sd-provider-author` skill if installed.

**3. Event sources become `#[trigger]`s** — channels/queues (`Queue`), timers
(`Cron`), signals (`Signal`/`Event`), and config-change reactions (`Watch`).

**4. Wire the daemon** with the builder, then run *and* block. `run()` is
non-blocking (it brings services up and returns); `wait()` is what keeps the
process alive. Binding `daemon` matters — dropping it tears everything down.

```rust
let daemon = ServiceDaemon::builder()
    .with_registry(registry) // optional: omit to run every discovered service
    .build();
daemon.run().await; // spawns services wave by wave, returns immediately
daemon.wait().await?; // blocks until SIGINT / SIGTERM / Ctrl+C
```

**5. Express ordering and recovery.** Give each service a `priority` so startup
runs high→low in waves and shutdown runs low→high. A service signals readiness with
`service_daemon::done()` (or implicitly via the first `is_shutdown()`/`sleep()`
call). State that must survive a restart goes on the daemon's per-service Shelf.
For cooperative latency-sensitive workers, declare `scheduling = HighPriority`
and let the daemon manage shard placement/scale-out internally. Do not use
HighPriority for blocking loops; use `Isolated` when a body needs a private
thread/runtime boundary.

## Don't

- Don't push framework policy (retry/backoff/restart) into business code; let the
  supervisor own it.
- Don't return `Arc<T>` from a provider — return plain `T`.
- Don't migrate everything at once; the daemon can host one service first, then
absorb the rest.

## Where to go next (the journey)

Each migration step has a dedicated mechanism skill — install them alongside this
one and follow the order:

| Step | Skill | What it covers |
| :--- | :--- | :--- |
| Wrap a loop as a service | `sd-service-author` | `#[service]` shape, readiness `done()`, interruptible `sleep`, restart semantics. |
| Model on-demand workers | `sd-service-author` + `sd-daemon-bootstrap` | `#[input]` service templates, `service_handle!`, and `ServiceHandle::create/start`. |
| Turn shared resources into providers | `sd-provider-author` | `#[provider]`, lazy vs `eager`, `ProviderError::Fatal` vs `Retryable`, DI forms. |
| React to events / timers / changes | `sd-trigger-author` | `#[trigger]` host families: `Queue` / `Cron` / `Signal` / `Watch`. |
| Share mutable state, persist across restart | `sd-state-management` | `Arc<RwLock<T>>`, `Watch` notifications, the keyed Shelf. |
| Assemble and run `main()` | `sd-daemon-bootstrap` | builder, tag-filtered `Registry`, `run()` vs `wait()`, priority waves. |

See `reference.md` for the detailed incremental migration recipe and
`pitfalls.md` for the traps people hit when porting an existing codebase.
