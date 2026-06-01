# `#[service]` reference

## 1. Signature

```rust
#[service]
pub async fn name(dep_a: Arc<A>, dep_b: Arc<B>) -> anyhow::Result<()> { /* ... */ }
```

- Every parameter is a dependency injected as `Arc<T>` for a `#[provider]` type.
  (Triggers, not services, are the place where a non-`Arc` payload parameter
  appears — see the `sd-trigger-author` skill.)
- Return `anyhow::Result<()>`. Returning a framework `ServiceError` (below) via
  `.into()` lets the supervisor classify the outcome.
- `async fn` is strongly preferred. A sync service must be annotated
  `#[allow(sync_handler)]` or it warns at runtime.

## 2. Lifecycle helpers (call from the service body)

| Helper | Purpose |
| :--- | :--- |
| `service_daemon::is_shutdown()` | `true` once shutdown is requested; loop condition. |
| `service_daemon::sleep(dur).await` | Interruptible sleep. Returns `false` if shutdown arrived during the wait — break the loop. |
| `service_daemon::done()` | Explicit readiness handshake: marks the service `Healthy`. |
| `service_daemon::wait_shutdown().await` | Await the shutdown signal. |

Use `service_daemon::sleep` rather than `tokio::time::sleep` so the service reacts
to shutdown immediately instead of finishing the full delay.

## 3. Readiness handshake

A service must reach `Healthy` so dependents in later startup waves can proceed.

- **Explicit**: call `service_daemon::done()` after initialization. Recommended for
  services with non-trivial setup.
- **Implicit**: for minimalist services, the first call to `is_shutdown()`,
  `sleep()`, or `wait_shutdown()` while the service is still in an introductory
  phase counts as the transition to `Healthy`.

## 4. Error and restart semantics

The supervisor classifies how a generation exits:

- `ServiceError::Fatal(..)` → service is `Terminated`; **no restart**.
- Ordinary `Err(..)` or a panic → `Recovering`; **restart with backoff**.
- Clean `Ok(())` → a fresh generation starts **immediately**; recorded as success,
  not a failure (so it does not accumulate backoff).
- A provider-init failure during lazy startup is treated as a daemon-wide boundary
  failure (the daemon shuts down).

Use a structured framework error on resource-acquisition paths so the supervisor
sees the right classification, e.g. `ServiceError::runtime_io("clone TCP listener", e)`
converted with `.into()`.

## 5. Priority and scheduling

- `priority` orders startup (high→low, in waves) and shutdown (low→high). Each wave
  waits for its services to report `Healthy`, bounded by `wave_spawn_timeout`.
- The body runs under a statically declared scheduling mode: `Standard` (host
  runtime), `HighPriority` (a dedicated low-contention lane), or `Isolated` (a
  private OS thread + runtime per generation). Default is `Standard`. Pick
  `Isolated`/`HighPriority` only for genuinely latency- or blocking-sensitive work.

(Exact builder/attribute wiring for priority and scheduling is covered by the
`sd-daemon-bootstrap` skill.)

## 6. State that must survive a restart

The daemon provides a per-service "Shelf" — state deposited there survives
generation termination and is inherited by the next generation of the same service.
For mutable shared state across services, inject managed state (`Arc<RwLock<T>>` /
`Arc<Mutex<T>>`); see the `sd-state-management` skill.
