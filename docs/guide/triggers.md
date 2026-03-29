Triggers are specialized services with built-in event loops that execute your functions when specific events occur. They consume zero CPU while waiting.

## 0. Quick Start: Chain Reactions

Triggers become powerful when they talk to each other. A trigger can "fire" another by calling a provider's method directly.

```rust
use service_daemon::prelude::*;

// 1. Define a Signal Provider
#[provider(Notify)]
pub struct CleanupSignal;

// 2. A trigger that performs work and notifies others
#[trigger(Queue(JobQueue))]
pub async fn worker(job: Job, signal: Arc<CleanupSignal>) -> anyhow::Result<()> {
    tracing::info!("Processing job {}", job.id);
    
    // Fire the signal directly via the DI-injected instance
    signal.notify();
    Ok(())
}

// 3. A reactive handler listening for that signal
#[trigger(Notify(CleanupSignal))]
pub async fn cleanup_handler() -> anyhow::Result<()> {
    tracing::info!("Cleaning up...");
    Ok(())
}
```

---

## 1. Architecture: Policy vs. Engine

Triggers follow a decoupled **Policy-Engine** architecture:

- **Engine (Generic)**: The `TriggerRunner` manages the main event loop, interceptor pipeline, and standard shutdown/reload handling. Built-in interceptors (`TracingInterceptor`, `RetryInterceptor`) provide tracing and retry for free. It's provided automatically by the framework.
- **Policy (Specific)**: Defines *how* to wait for the next event. Each trigger type (Cron, Queue, etc.) implements its own policy via the `TriggerHost` trait's `setup` (one-time initialization) and `handle_step` (per-event waiting) methods.

### The `TriggerTransition` Protocol
Policies communicate with the engine using a transition enum:
- `Next(payload)`: Dispatch the event and continue the loop.
- `Reload(payload)`: Dispatch the event, then enter an idle state, waiting for the framework to restart the service (used by state-based triggers like `Watch`).
- `Stop`: Terminate the trigger loop cleanly.


## 1. Trigger Template Reference

| Template | Alias | Functionality |
| :--- | :--- | :--- |
| `Cron` | - | Time-based scheduling via `tokio-cron-scheduler` |
| `Queue` | `BQueue`, `BroadcastQueue` | Receives every message sent to the target queue |
| `Watch` | `State` | Zero-lock reactive handlers for shared state changes |
| `Notify` | `Event`, `Signal` | Simple signal-based triggers |

## 2. Detailed Usage

### Cron Trigger
```rust
#[trigger(Cron(CleanupSchedule))]
async fn hourly_cleanup() -> anyhow::Result<()> {
    tracing::info!("Cleaning up...");
    Ok(())
}
```

### Queue Triggers

```rust
#[trigger(Queue(WorkerQueue))]
async fn worker(item: Task) -> anyhow::Result<()> { ... }
```

### Watch Trigger (State Change)
Executes automatically whenever shared state (`Arc<RwLock<T>>` or `Arc<Mutex<T>>`) is modified. Internally, this leverages the `ServiceDaemon`'s reload mechanism: the service is re-spawned with a fresh snapshot exactly when the state changes.

```rust
#[trigger(Watch(MyData))]
pub async fn on_data_changed(snapshot: Arc<MyData>) -> anyhow::Result<()> {
    tracing::info!("New value: {}", snapshot.value);
    Ok(())
}
```

### Priority
All triggers support the `priority` parameter for wave-based startup/shutdown ordering:
```rust
#[trigger(Watch(MetricsData), priority = 80)]
pub async fn on_metrics_changed(snapshot: Arc<MetricsData>) -> anyhow::Result<()> { ... }
```

## 3. Parameter Mapping Rules

1. **Implicit Payload**: The first parameter that is *not* an `Arc<T>` is treated as the event payload.
2. **Explicit Payload**: Any parameter marked with `#[payload]` is the payload (allows `Arc<Payload>`).
3. **DI Resources**: All other `Arc<T>` parameters are resolved via the DI system.

## 4. Event Flow: Causal Tracing

Services and triggers emit events by calling provider instance methods directly (e.g. `notifier.notify()`, `queue.push(...)`) after resolving the provider via DI. 

The framework's `TriggerRunner` automatically manages the **Causal Identity** for every dispatched event:
1.  **Message ID** (UUID v7): A time-ordered, globally unique ID for the event.
2.  **Source ID**: The `ServiceId` of the service that originally fired the event.
3.  **Service ID**: The `ServiceId` of the current trigger handler.
4.  **Instance Seq**: A monotonic sequence number for the current invocation.

This 4-tuple identity enables structured log correlation and automated topology mapping without manual intervention.

## 5. Resilience: Automatic Handler Retries

Individual trigger handler failures (returning `Err`) are automatically retried using the same global **Exponential Backoff** policy as regular services.

### How it works
When a handler fails:
1. The built-in `RetryInterceptor` catches the error and manages retry logic with a `BackoffController`.
2. The payload is shared via `Arc` internally -- retries **never** deep-copy business data.
3. Errors are automatically logged with structured context.
4. Shutdown signals are respected during backoff waits -- no hanging retries.

The retry logic is implemented as an interceptor layer, part of the composable `TriggerInterceptor` pipeline. See [Interceptor Middleware](interceptor-middleware.md) for details on customizing the dispatch pipeline.

### Payload Handling

The framework wraps every payload in `Arc<P>` at the dispatch boundary. How the payload reaches your handler depends on your function signature:

| Handler Signature | What Happens | `Clone` Required? |
|:---|:---|:---|
| `async fn handler(data: T)` | Macro auto-clones from `Arc` | **Yes** |
| `async fn handler(#[payload] data: Arc<T>)` | Zero-copy pointer pass | **No** |

> [!TIP]
> For large payloads or types that cannot implement `Clone`, declare your handler parameter as `Arc<T>`. This gives you true zero-copy access and works with any type.

---

## 6. Elastic Scaling (Async Dispatch)

Elastic scaling is **automatically enabled** only for streaming trigger templates that declare scaling support (e.g. `Queue` / `TopicHost`). Other templates (`Cron`, `Watch`, `Notify`) dispatch handlers serially with zero scaling overhead.

Each trigger template declares its scaling needs via `TriggerHost::scaling_policy()`. Users can override the template defaults using `ServiceDaemonBuilder::with_trigger_config(ScalingPolicy::builder()...build())`.

---

## 7. Instance Lifecycle & State Reuse

Unlike standard services where the macro-wrapped function is re-executed on every iteration, triggers leverage a **Stateful Host** model:

1.  **Instantiation**: The `TriggerHost` is created **once** via `setup()` when the service starts.
2.  **State Persistence**: The `TriggerRunner` maintains a reference to this instance and calls `handle_step(&mut self, ...)` in a loop.
3.  **State Reuse**: You can store resources (e.g., a `tokio::sync::mpsc::Receiver` or a local cache) as struct fields in your `TriggerHost`. These fields are preserved across all event iterations.
4.  **Reload Boundary**: When a reload signal is received (e.g., configuration change), the current `TriggerRunner` and its `TriggerHost` are dropped, and a **new** instance is created.

This design enables high-performance event processing by avoiding repeated setup overhead while ensuring clean resource isolation during reloads.

---

### 8. Elastic Scaling & Backpressure Details

Elastic scaling is governed by the [`ScalingPolicy`]. The framework automatically adjusts concurrency based on pressure and ensures backpressure via a shared semaphore.

> [!NOTE]
> **Scaling Internals**: For the mathematical pressure formula, 1-second monitoring logic, and the "Shadow Permits" implementation details, see [Internal Architecture: Trigger Scaling](../architecture/internal-overview.md#7-coretrigger_runner_rs).

---

## 7. More Information

- [Provider Best Practices](provider-best-practices.md): Deep dive into defining custom providers.
- [Concept Clarification (FAQ)](pitfalls-faq.md#2-lifecycle--paradigms): Understanding the difference between managed triggers and standard services.

[Back to README](../../README.md)
