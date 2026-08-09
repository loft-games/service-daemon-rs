# Event Triggers

Triggers are specialized services with built-in event loops that run your function when an event occurs. While idle they are blocked on the underlying primitive (channel `recv`, `Notify::notified()`, etc.) -- no polling, no busy-wait.

## 0. Quick Start: Chain Reactions

Triggers can compose into chains: one trigger fires the next by calling a provider method that another trigger is listening on.

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

- **Engine (Generic)**: The `TriggerRunner` manages the main event loop, interceptor pipeline, and standard shutdown/reload handling. Built-in interceptors (`TracingInterceptor`, `RetryInterceptor`) provide tracing and retry.
- **Policy (Specific)**: Defines *how* to wait for the next event. Each trigger type (Cron, Queue, etc.) implements its own policy via the `TriggerHost` trait's `setup` (one-time initialization) and `handle_step` (per-event waiting) methods.

### The `TriggerTransition` Protocol
Policies communicate with the engine using a transition enum:
- `Next(payload)`: Dispatch the event and continue the loop.
- `Reload(payload)`: Dispatch the event, then enter an idle state, waiting for the framework to restart the service (used by state-based triggers like `Watch`).
- `Stop`: Terminate the trigger loop cleanly.


## 2. Trigger Template Reference

| Template | Alias | Functionality |
| :--- | :--- | :--- |
| `Cron` | - | Time-based scheduling via `tokio-cron-scheduler` |
| `Queue` | `BQueue`, `BroadcastQueue` | Receives every message sent to the target queue |
| `Watch` | `State` | Runs when watched provider state changes |
| `Notify` | `Event`, `Signal` | Simple signal-based triggers |

## 3. Detailed Usage

### Cron Trigger

Cron triggers receive a provider type that resolves to the cron expression.

```rust
#[derive(Clone)]
#[provider("0 0 * * * *")]
pub struct CleanupSchedule(pub String);

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
Executes automatically whenever shared state (`Arc<RwLock<T>>` or `Arc<Mutex<T>>`) is modified. Internally, this uses the `ServiceDaemon` reload mechanism: the service is re-spawned with a fresh snapshot exactly when the state changes.

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

### Scheduling

Triggers also support the same static `scheduling` declaration as services. The trigger host still defines how events are received; scheduling declares the execution mode used by the generated trigger service body while supervision, watchers, reload, restart/backoff, and shutdown coordination stay on the daemon control plane.

```rust
#[trigger(Queue(WorkerQueue), scheduling = HighPriority)]
async fn urgent_worker(item: Task) -> anyhow::Result<()> { ... }
```

Use `Standard` by default, `HighPriority` for latency-sensitive trigger dispatch, and `Isolated` only when the trigger loop body needs a dedicated OS thread and private Tokio runtime. `HighPriority` is not an overflow pool for ordinary triggers; a trigger uses that lane only when its source declaration asks for it. `Isolated` trigger bodies still report outcomes through the daemon supervisor, so reload, restart/backoff, and shutdown coordination remain daemon-managed.

## 4. Parameter Mapping Rules

1. **Implicit Payload**: The first parameter that is *not* an `Arc<T>` is treated as the event payload.
2. **Explicit Payload**: Any parameter marked with `#[payload]` is the payload (allows `Arc<Payload>`).
3. **DI Resources**: All other `Arc<T>` parameters are resolved via the DI system.

## 5. Event Flow: Causal Tracing

Services and triggers emit events by calling provider instance methods directly (e.g. `notifier.notify()`, `queue.push(...)`) after resolving the provider via DI. 

The framework's `TriggerRunner` automatically manages the **Causal Identity** for every dispatched event:
1.  **Message ID** (UUID v7): A time-ordered, globally unique ID for the event.
2.  **Source service instance ID**: The `ServiceInstanceId` of the service instance that originally fired the event.
3.  **Current service instance ID**: The `ServiceInstanceId` of the current trigger handler.
4.  **Instance sequence**: A monotonic sequence number for the current invocation.

This 4-tuple identity supports log correlation and trace reconstruction.

## 6. Resilience: Automatic Handler Retries

Individual trigger handler failures (returning `Err`) are automatically retried using the daemon's **Exponential Backoff** policy. A single failed attempt is treated as a message-handling problem, not as a failed service generation.

### How it works
When a handler fails:
1. The built-in `RetryInterceptor` catches the error and manages retry logic with a `BackoffController`.
2. The payload is shared via `Arc` internally -- retries **never** deep-copy business data.
3. Errors are automatically logged with structured context.
4. Shutdown and reload signals interrupt backoff waits cleanly -- no hanging retries and no false failure report during lifecycle cancellation.

If you set `trigger_max_retries` on `RestartPolicy`, reaching that limit means the current dispatch has been exhausted. At that point the trigger service generation reports a recoverable failure to the normal supervisor, so the existing restart/backoff/status/diagnostics path is used. Dispatch infrastructure failures, such as a spawned dispatch task panic, also flow through the supervisor; panic is recorded as a panic exit rather than as a silent log line.

Application code usually only configures the restart policy and writes idempotent handlers.

### Payload Handling

The framework wraps every payload in `Arc<P>` at the dispatch boundary. How the payload reaches your handler depends on your function signature:

| Handler Signature | What Happens | `Clone` Required? |
|:---|:---|:---|
| `async fn handler(data: T)` | Macro auto-clones from `Arc` | **Yes** |
| `async fn handler(#[payload] data: Arc<T>)` | Zero-copy pointer pass | **No** |

> [!TIP]
> For large payloads or types that cannot implement `Clone`, declare your handler parameter as `Arc<T>`. This passes the payload by shared pointer and works with any type.

---

## 7. Queue concurrency (async dispatch)

Concurrent dispatch is enabled only for streaming trigger templates that declare scaling support (e.g. `Queue` / `TopicHost`). Other templates (`Cron`, `Watch`, `Notify`) dispatch handlers serially.

Each trigger template declares its scaling needs via `TriggerHost::scaling_policy()`. Most users should keep the template defaults; when you need to override them, use `ScalingPolicy::builder()` and pass the result to `ServiceDaemonBuilder::with_trigger_config(...)`.

```rust
use std::time::Duration;

use service_daemon::{ScalingPolicy, ServiceDaemon};

let scaling = ScalingPolicy::builder()
    .initial_concurrency(4)
    .max_concurrency(64)
    .scale_factor(2)
    .scale_threshold(5)
    .scale_cooldown(Duration::from_secs(30))
    .build();

let mut daemon = ServiceDaemon::builder()
    .with_trigger_config(scaling)
    .build();
```

Handlers can request a temporary policy overlay for future events handled by the
same trigger generation. The request is self-scoped from `TriggerContext`, so the
handler supplies only the overlay itself:

```rust
use std::time::Duration;

use service_daemon::{RestartPolicy, TriggerContext, TriggerPolicyOverlay};

async fn on_event(ctx: TriggerContext<MyEvent>) -> anyhow::Result<()> {
    if let Some(pressure) = ctx.pressure()
        && pressure.in_flight >= pressure.current_limit
    {
        let overlay = TriggerPolicyOverlay::builder("downstream pressure", Duration::from_secs(30))
            .concurrency_limit(2)
            .dispatch_timeout(Duration::from_secs(5))
            .retry_policy(
                RestartPolicy::builder()
                    .initial_delay(Duration::from_millis(100))
                    .max_delay(Duration::from_secs(2))
                    .trigger_max_retries(3)
                    .build(),
            )
            .build()?;

        ctx.request_policy_overlay(overlay)?;
    }

    Ok(())
}
```

Temporary overlays require a non-empty reason and a TTL. They affect future
dispatch boundaries only; a dispatch that already captured its timeout or retry
policy keeps that policy. The runner clears overlays on TTL expiry or when the
trigger generation ends.

Custom `TriggerHost::run_as_service` implementations that bypass the default
runner should construct contexts with `TriggerContext::new(...)` and pass the
current service instance ID and generation.

---

## 8. Instance Lifecycle & State Reuse

Unlike standard services where the macro-wrapped function is re-executed on every iteration, triggers use a **Stateful Host** model:

1.  **Instantiation**: The `TriggerHost` is created **once** via `setup()` when the service starts.
2.  **State Persistence**: The `TriggerRunner` maintains a reference to this instance and calls `handle_step(&mut self, ...)` in a loop.
3.  **State Reuse**: You can store resources (e.g., a `tokio::sync::mpsc::Receiver` or a local cache) as struct fields in your `TriggerHost`. These fields are preserved across all event iterations.
4.  **Reload Boundary**: When a reload signal is received (e.g., configuration change), the current `TriggerRunner` and its `TriggerHost` are dropped, and a **new** instance is created.

This avoids repeated setup work while keeping reload boundaries clear.

---

## 9. Concurrency and backpressure details

Queue concurrency is governed by the [`ScalingPolicy`]. The framework adjusts concurrency based on pressure and applies backpressure through a shared semaphore. Most applications only need the builder example above; custom host and diagnostic internals are covered in the architecture docs.

---

## 10. More Information

- [Provider Strategy Guide](provider-best-practices.md): How to define custom providers.
- [Concept Clarification (FAQ)](faq.md#2-lifecycle--paradigms): Understanding the difference between managed triggers and standard services.

[Back to README](../../README.md)
