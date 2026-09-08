# `#[service]` reference

## 1. Signature

```rust
#[service]
pub async fn name(dep_a: Arc<A>, dep_b: Arc<B>) -> anyhow::Result<()> { /* ... */ }
```

- Most parameters are dependencies injected as `Arc<T>`, `Arc<RwLock<T>>`, or
  `Arc<Mutex<T>>` for `#[provider]` types.
- A service template may declare **one** startup input parameter:
  `#[input] cfg: &Config`. It must be an immutable reference with lifetime
  elision, not `&mut`, not `Arc<T>`, and not combined with `#[payload]`.
- Bare non-`Arc` parameters without `#[input]` are rejected for services. Trigger
  payload parameters remain trigger-only; see the `sd-trigger-author` skill.
- Return `anyhow::Result<()>`. Returning a framework `ServiceError` (below) via
  `.into()` lets the supervisor classify the outcome.
- `async fn` is strongly preferred. A sync service must be annotated
  `#[allow(sync_handler)]` or it warns at runtime.

### Service templates and on-demand instances

```rust
pub struct WorkerConfig {
    pub id: u64,
    pub heartbeat_interval: std::time::Duration,
}

#[service(tags = ["on-demand"])]
pub async fn worker(#[input] config: &WorkerConfig) -> anyhow::Result<()> {
    service_daemon::done();
    while service_daemon::sleep(config.heartbeat_interval).await {
        // work for this instance
    }
    Ok(())
}
```

Declaring `#[input]` makes the service a template: it is selected by `Registry`,
but no instance is created during daemon startup. Resolve a daemon-bound
`ServiceHandle` with `service_handle!(path::to::worker)` in a provider, wrap it
in a provider type, and inject that wrapper into whichever controller service
needs to spawn workers. Then call:

- `handle.create(input).await` to register an instance without starting it.
- `handle.start(input).await` to create and start an instance.
- `instance.start().await`, `stop().await`, `remove().await`, or
  `request_stop()` for instance-level control.

The input value is owned by the service instance record and reused across restart
or reload generations for that instance. Different created instances own distinct
input allocations.

For services without `#[input]`, the selected definition auto-starts one instance
during daemon startup. Calling `ServiceHandle::create` on such a handle is only
valid with unit input (`create(())`); non-unit input is rejected because the
service did not declare startup input.

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
- Clean `Ok(())` without shutdown or reload → `NormalExit`; a fresh generation
  starts through `RestartPolicy` normal-exit backoff. Services are expected to be
  long-running, so an unprompted clean return is treated as unexpected lifecycle
  termination even though the Rust result is `Ok`.
- Clean `Ok(())` after reload → immediate generation replacement.
- Clean `Ok(())` after shutdown → terminal shutdown path.
- A provider-init failure during lazy startup is treated as a daemon-wide boundary
  failure (the daemon shuts down).
- A service-template input type mismatch at `create(input)`/`start(input)` is a
  runtime validation error before the instance starts.

Use a structured framework error on resource-acquisition paths so the supervisor
sees the right classification, e.g. `ServiceError::runtime_io("clone TCP listener", e)`
converted with `.into()`.

## 5. Priority and scheduling

- `priority` orders startup (high→low, in waves) and shutdown (low→high). Each wave
  waits for its services to report `Healthy`, bounded by `wave_spawn_timeout`.
- The body runs under a statically declared scheduling mode: `Standard` (host
  runtime), `HighPriority` (framework-owned low-contention runtime shards), or
  `Isolated` (a private OS thread + runtime per generation). Default is
  `Standard`. Pick `HighPriority` for short cooperative work with latency needs;
  pick `Isolated` for genuinely blocking or thread-affine work.
- HighPriority placement is automatic inside the declared mode. The daemon's
  internal HighPriority control loop may create additional shards and may request
  cooperative rollover. Rollover uses the reload signal path: write services so
  they can reach a reload-safe point and store needed progress in the Shelf or
  managed providers before exiting.

Enable `high-priority` in the dependency's Cargo features before declaring this
mode. The controller uses fresh completed ServiceSleep drift with supporting
shard pressure, then compares the same metric after actual new-generation
placement. It pauses repeated low-benefit interventions; it does not promise
hard real-time latency. There are no global public tuning setters or required
round-observation APIs. Services without valid sleep observations do not drive
the initial feedback path. All services are reloadable without a separate
eligibility declaration; saving/restoring in-flight business progress is the
author's responsibility, not a framework guarantee of lossless reload.

(Exact builder/attribute wiring for priority and scheduling is covered by the
`sd-daemon-bootstrap` skill.)

## 6. State that must survive a restart

The daemon provides a per-service "Shelf" — state deposited there survives
generation termination and is inherited by the next generation of the same service.
For mutable shared state across services, inject managed state (`Arc<RwLock<T>>` /
`Arc<Mutex<T>>`); see the `sd-state-management` skill.
