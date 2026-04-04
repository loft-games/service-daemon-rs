# Priorities & Scheduling Policies

In a large system, order matters. You can't start your API Gateway before your Database is ready, and you shouldn't shut down your metrics logger until everything else has finished reporting.

`service-daemon-rs` gives you two separate knobs:

- **`priority`** controls **startup and shutdown order**.
- **`scheduling`** controls **which runtime lane the service uses**.

Use them together: let priorities express lifecycle dependencies, and let scheduling choose between the shared standard runtime, the shared high-priority runtime, and isolated execution.

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

Priority decides when a service starts and stops. Scheduling decides whether it runs on the standard shared runtime, the shared high-priority runtime, or an isolated thread.

```rust,ignore
use service_daemon::{service, ServicePriority};

#[service]
async fn standard_worker() -> anyhow::Result<()> {
    Ok(())
}

#[service(priority = ServicePriority::SYSTEM, scheduling = HighPriority)]
async fn latency_sensitive_supervisor() -> anyhow::Result<()> {
    Ok(())
}

#[service(priority = ServicePriority::STORAGE, scheduling = Isolated)]
async fn modbus_server() -> anyhow::Result<()> {
    Ok(())
}
```

### `Standard`

`Standard` is the default.

- Runs on the shared multi-threaded Tokio runtime.
- Best for most background services and triggers.
- Use this unless you have a concrete reason to prefer another mode.

```rust,ignore
#[service(scheduling = Standard)]
async fn admin_service() -> anyhow::Result<()> {
    Ok(())
}
```

### `HighPriority`

`HighPriority` runs the service on the daemon's shared high-priority runtime.

- Use it for latency-sensitive work that should stay on the shared runtime, but not compete with the standard lane.
- It is distinct from `Isolated`, which creates a private OS thread and Tokio runtime.

```rust,ignore
#[service(scheduling = HighPriority)]
async fn watchdog_service() -> anyhow::Result<()> {
    Ok(())
}
```

### `Isolated`

`Isolated` runs the service on a dedicated OS thread with its own Tokio runtime.

- Best for deterministic loops, blocking adapters, or workloads that should not contend with the shared runtime.
- Useful for things like tight polling intervals, device I/O bridges, or thread-affine integrations.
- Comes with a higher runtime cost than `Standard`, so use it deliberately.

```rust,ignore
#[service(priority = ServicePriority::STORAGE, scheduling = Isolated)]
async fn modbus_server() -> anyhow::Result<()> {
    Ok(())
}
```

The `examples/scheduling` demo shows the `Standard` and `Isolated` ends of the split in practice; `HighPriority` follows the same shared-runtime pattern as `Standard`, but on the high-priority lane.

## 5. Why This Split?

- **Dependency Safety**: Your business logic can safely assume the database is ready because it's in a higher priority wave.
- **Latency Control**: You can isolate a hot loop without changing its startup order.
- **Log Integrity**: You'll never miss a "Shutdown Complete" log because the logging system is the last thing to stop.
- **Predictable Lifecycle**: No more race conditions where components die in a random order.

---

[**<- Previous Step: Error Handling & Retries**](./error-handling.md) | [**Next Step: Unit Testing & Simulation ->**](./unit-testing.md)
