# Priorities & Scheduling Policies

In a large system, order matters. You can't start your API Gateway before your Database is ready, and you shouldn't shut down your metrics logger until everything else has finished reporting.

`service-daemon-rs` gives you two separate knobs:

- **`priority`** controls **startup and shutdown order**.
- **`scheduling`** controls **which runtime lane the service or trigger body uses**.

Use them together: let priorities express lifecycle dependencies, and let scheduling declare whether the body uses the host Tokio runtime, the daemon-owned high-priority runtime lane, or isolated body execution. The daemon's internal control runtime still owns supervision and lifecycle orchestration.

---

## 1. Setting Priorities

Every service and trigger has a priority. The default is `50`. High numbers mean "more important".

```rust,ignore
use service_daemon::ServicePriority;

#[service(priority = ServicePriority::SYSTEM)] // 100
async fn logger_service() { ... }

#[service(priority = ServicePriority::STORAGE)] // 80
async fn database_pool() { ... }

// You can also use raw u8 numbers!
#[service(priority = 60)]
async fn important_worker() { ... }

#[service(priority = ServicePriority::DEFAULT)] // 50
async fn business_logic() { ... }

#[service(priority = ServicePriority::EXTERNAL)] // 0
async fn web_api() { ... }
```

### The Priority Value

Under the hood, `priority` is a simple **`u8`** value. You are not limited to the pre-defined constants. Feel free to use any number between `0` and `255` to fine-tune your startup waves.

## 2. Startup: High to Low

When the daemon starts, it groups services into waves based on their priority.

1. **Wave 100** starts first. The daemon waits for services in this wave to reach `Healthy` (by calling `done()` or hitting a lifecycle helper).
2. That wait is bounded by `wave_spawn_timeout`. If the timeout expires, the daemon logs a warning and still starts the next wave instead of blocking startup forever.
3. **Wave 80** then starts, followed by lower waves down to **Wave 0**.

## 3. Shutdown: Low to High

When you stop the system (Ctrl+C), the process reverses. We want to stop the "outer" layers first to prevent new requests from entering while we clean up.

1. **Wave 0** is stopped first. The daemon signals these services and waits for them to exit.
2. **Wave 50** is stopped next.
3. ...finally, **Wave 100** (Logging/Metrics) is the last to go, ensuring we capture all logs from the shutdown process.

## 4. Choosing a Scheduling Policy

Priority decides when a service or trigger starts and stops. Scheduling is a static source-level declaration that decides whether its execution body uses the host Tokio runtime, the daemon-owned high-priority runtime lane, or an isolated thread. Supervision, watchers, startup/shutdown waves, reload, and restart/backoff orchestration stay on the daemon's internal control runtime.

Diagnostics track the control plane and body execution lanes separately as `Control`, `Standard`, `HighPriority`, and `Isolated`. `Control` is an internal diagnostics lane, not a user-facing scheduling policy. The observations are diagnostic only: they distinguish lane pressure and generation outcomes, but they do not change the declared scheduling mode or migrate services automatically.

Scheduling advice is recommendation-first. The internal analyzer may log advice such as `Observe`, `InvestigateControlPlane`, `ConsiderIsolation`, or `KeepCurrentLane`, but those are investigation labels, not executable migration commands. `HighPriority` is not an automatic overflow pool for ordinary services, and `Isolated` carries extra OS thread/runtime allocation cost.

| Signal type | Examples | Interpretation |
| :--- | :--- | :--- |
| Useful diagnostic signals | Service sleep drift, runtime probe drift, restart/backoff observations, isolated startup pressure | Investigate runtime pressure, lifecycle ownership, or whether the source-level scheduling declaration should be changed in a future build. |
| Mode-internal planning | Declared `HighPriority` services or triggers in the final registry | Phase 7 derives the high-priority runtime worker count at initialization from those entries. |
| Future research only | Sustained pressure that remains after static HighPriority planning | Runtime epoch rollover is deferred to Phase 9 research and would still stay inside the declared `HighPriority` mode. |
| Never automatic from this signal alone | Control lane pressure, HighPriority saturation, restart storms, isolated startup pressure, or one service's high sleep drift | Do not infer cross-mode migration; sleep drift identifies pressure, not a sole culprit. |

`SchedulingAdvisoryProfile` controls only whether this advisory emission runs. The default keeps it enabled; `SchedulingAdvisoryProfile::disabled()` stops advisory emission without changing declared scheduling modes, body placement, reload, restart, or shutdown behavior.

```rust,ignore
use service_daemon::prelude::*;
use service_daemon::{provider, service, trigger};

#[derive(Clone)]
struct Job;

#[provider(Queue(Job))]
struct JobQueue;

#[service]
async fn standard_worker() -> anyhow::Result<()> {
    Ok(())
}

#[service(priority = ServicePriority::SYSTEM, scheduling = HighPriority)]
async fn latency_sensitive_worker() -> anyhow::Result<()> {
    Ok(())
}

#[service(priority = ServicePriority::STORAGE, scheduling = Isolated)]
async fn modbus_server() -> anyhow::Result<()> {
    Ok(())
}

#[trigger(Queue(JobQueue), priority = ServicePriority::DEFAULT, scheduling = HighPriority)]
async fn urgent_job_worker(job: Job) -> anyhow::Result<()> {
    Ok(())
}
```

### `Standard`

`Standard` is the default.

- Runs on the host Tokio runtime that calls `ServiceDaemon::run()`.
- Best for most background services and triggers.
- Use this unless you have a concrete reason to declare another mode.

```rust,ignore
#[service(scheduling = Standard)]
async fn admin_service() -> anyhow::Result<()> {
    Ok(())
}
```

### `HighPriority`

`HighPriority` runs the service or trigger body on the daemon-owned low-contention high-priority runtime lane. The supervisor and lifecycle control path still run on the internal control runtime.

- Use it for latency-sensitive work that should stay on a shared framework-owned runtime, but not compete with the standard body lane.
- The runtime is created lazily by `ServiceDaemon::run()` only when the final registry contains at least one `HighPriority` service or trigger.
- Its worker count is planned from the final registry's declared `HighPriority` entry count, capped by available host parallelism.
- HighPriority services and HighPriority triggers count as the same kind of registry entry; there is no trigger/service weighting model.
- `ServiceDaemonBuilder::build()` does not create this runtime, so applications that only use `Standard` and `Isolated` do not pay the extra shared runtime cost.
- It is not an overflow pool for `Standard`; a service enters this lane only by declaring `scheduling = HighPriority` in source.
- It is distinct from `Isolated`, which creates a private OS thread and Tokio runtime for each service generation body.
- Runtime epoch rollover is deferred to Phase 9 research and would remain inside the declared `HighPriority` mode.

```rust,ignore
#[service(scheduling = HighPriority)]
async fn watchdog_service() -> anyhow::Result<()> {
    Ok(())
}
```

### `Isolated`

`Isolated` runs each service or trigger generation body on a dedicated OS thread with its own Tokio runtime.

- Best for deterministic loops, blocking adapters, or workloads that should not contend with the shared runtime.
- Useful for things like tight polling intervals, device I/O bridges, or thread-affine integrations.
- The daemon still owns supervision, reload signaling, restart/backoff, and shutdown coordination.
- A daemon-level admission gate limits concurrent isolated startup allocation so resource pressure does not create a thread/runtime creation storm.
- Configure the gate with `ServiceDaemonBuilder::with_isolated_startup_concurrency_limit(NonZeroUsize)` when the default is too high or too low.
- The gate only covers permit acquisition, OS thread spawn, and private Tokio runtime build; once the generation body starts, it does not limit the body's lifetime.
- Startup allocation failures are recoverable isolated startup failures and use the same backoff/storm-guard path as other recoverable service failures.
- Comes with a higher runtime cost than `Standard`, so use it deliberately.

```rust,ignore
#[service(priority = ServicePriority::STORAGE, scheduling = Isolated)]
async fn modbus_server() -> anyhow::Result<()> {
    Ok(())
}
```

The `examples/scheduling` demo shows all three policies in practice: `Standard`, `HighPriority`, and `Isolated`.

Scheduling is part of the static registry entry generated by both `#[service]` and `#[trigger]`. It is a declared execution mode, not a runtime hint or public override surface; changing it means changing the source declaration and rebuilding/redeploying. A trigger's host still controls how it waits for events, while daemon supervision stays on the control plane for every scheduling mode.

Per-service restart policy overrides and scheduling hints are deferred from Phase 6. Restart policy remains daemon-level, and any future scheduling hint must be limited to the declared mode's internal strategy; it must not change `Standard`, `HighPriority`, or `Isolated`.

## 5. Why This Split?

- **Dependency Safety**: Your business logic can safely assume the database is ready because it's in a higher priority wave.
- **Latency Control**: You can isolate a hot loop without changing its startup order.
- **Log Integrity**: You'll never miss a "Shutdown Complete" log because the logging system is the last thing to stop.
- **Predictable Lifecycle**: No more race conditions where components die in a random order.

---

[**<- Previous Step: Error Handling & Retries**](./error-handling.md) | [**Next Step: Unit Testing & Simulation ->**](./unit-testing.md)
