# Priorities & Scheduling Policies

In a large system, order matters. You can't start your API gateway before your database is ready, and you shouldn't shut down your metrics logger until everything else has finished reporting.

`service-daemon-rs` gives you two separate knobs:

- **`priority`** controls **startup and shutdown order**.
- **`scheduling`** controls **where the service or trigger body runs**.

Use priorities for lifecycle dependencies. Use scheduling only when a body has a clear runtime-placement need.

---

## 1. Setting Priorities

Every service and trigger has a priority. The default is `50`. High numbers start earlier and stop later.

```rust,ignore
use service_daemon::ServicePriority;

#[service(priority = ServicePriority::SYSTEM)] // 100
async fn logger_service() { ... }

#[service(priority = ServicePriority::STORAGE)] // 80
async fn database_pool() { ... }

#[service(priority = 60)]
async fn important_worker() { ... }

#[service(priority = ServicePriority::DEFAULT)] // 50
async fn business_logic() { ... }

#[service(priority = ServicePriority::EXTERNAL)] // 0
async fn web_api() { ... }
```

The built-in constants are just named `u8` values. Use them when they fit; use a raw number when your system needs a more precise startup wave.

## 2. Startup: High to Low

When the daemon starts, it groups services into waves based on priority.

1. **Wave 100** starts first. The daemon waits for services in this wave to reach `Healthy` by calling `done()` or hitting a lifecycle helper.
2. If `wave_spawn_timeout` expires, the daemon logs a warning and continues with the next wave instead of blocking startup forever.
3. **Wave 80** then starts, followed by lower waves down to **Wave 0**.

## 3. Shutdown: Low to High

Shutdown runs in the opposite order. External-facing services stop first so they stop accepting new work while inner systems finish cleanup.

1. **Wave 0** stops first.
2. **Wave 50** stops next.
3. **Wave 100** stops last, which is useful for logging, metrics, and other core observers.

## 4. Choosing a Scheduling Policy

Priority decides *when* something starts and stops. Scheduling decides *where* the service or trigger body runs.

| Mode | Best for | Tradeoff |
| :--- | :--- | :--- |
| `Standard` | Most services and triggers | Uses the host Tokio runtime; this is the default and should be your first choice. |
| `HighPriority` | Short, cooperative, latency-sensitive work | Uses framework-owned high-priority runtime shards. The daemon can add conservative capacity, but it is not an overflow pool for ordinary work. |
| `Isolated` | Blocking adapters, thread-affine integrations, deterministic hot loops | Uses a private OS thread and private Tokio runtime for each generation, so it costs more resources. |

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

- Use it for normal async services and triggers.
- It runs on the Tokio runtime that calls the daemon handle's `run()`.
- Prefer this unless you have a concrete reason to declare another mode.

```rust,ignore
#[service(scheduling = Standard)]
async fn admin_service() -> anyhow::Result<()> {
    Ok(())
}
```

### `HighPriority`

`HighPriority` is for work that should avoid contention with the default body lane but still behaves like normal cooperative async Rust. The daemon starts with capacity derived from declared HighPriority services/triggers and, by default, runs a conservative HighPriority runtime policy that can add shards up to the available-parallelism cap when shard probe drift is sustained.

- Good for watchdogs, latency-sensitive queues, and small coordination tasks.
- Not a way to make CPU-heavy or blocking code safe.
- Not a cross-mode overflow pool; a body enters this lane only when you declare `scheduling = HighPriority`.
- Existing futures are not migrated. Placement changes happen only when a new generation starts. If the policy requests rollover, the service must observe the reload signal at a safe point and preserve any needed state with normal framework tools such as the Shelf.

```rust,ignore
#[service(scheduling = HighPriority)]
async fn watchdog_service() -> anyhow::Result<()> {
    Ok(())
}
```

There is no macro attribute or daemon builder knob for tuning the automatic
HighPriority placement loop. Treat `HighPriority` as a framework-managed
cooperative lane: if a service cannot tolerate cooperative rollover, keep its
work restart-safe with normal lifecycle tools or choose a different scheduling
mode.

### `Isolated`

`Isolated` gives each generation body its own OS thread and Tokio runtime.

- Good for blocking adapters, thread-affine libraries, or deterministic loops that should not contend with shared runtime work.
- More expensive than `Standard`, so use it deliberately.
- The daemon still owns restart, reload, and shutdown behavior.

```rust,ignore
#[service(priority = ServicePriority::STORAGE, scheduling = Isolated)]
async fn modbus_server() -> anyhow::Result<()> {
    Ok(())
}
```

The `examples/scheduling` demo shows all three policies in practice.

## 5. Why This Split?

- **Dependency Safety**: Your business logic can safely assume the database is ready because it's in a higher priority wave.
- **Latency Control**: You can isolate a hot loop without changing its startup order.
- **Log Integrity**: You'll never miss a shutdown log because the logging system can stop last.
- **Predictable Lifecycle**: Services start and stop in waves instead of racing each other.

---

[**<- Previous Step: Error Handling & Retries**](./error-handling.md) | [**Next Step: Unit Testing & Simulation ->**](./unit-testing.md)
