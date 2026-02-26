# Event Triggers

Triggers are specialized services with built-in event loops that execute your functions when specific events occur.

## 0. Architecture: Policy vs. Engine

Starting from v0.1.0, triggers follow a decoupled **Policy-Engine** architecture:

- **Engine (Generic)**: Handles the main event loop, tracing, monotonically increasing instance IDs, and standard shutdown/reload logic. It's provided automatically by the framework.
- **Policy (Specific)**: Defines *how* to wait for the next event. Each trigger type (Cron, Queue, etc.) implements its own policy via the `handle_step` method.

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
| `LBQueue` | `LoadBalancingQueue` | Distributes messages to one available worker at a time |
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
- **Broadcast (`Queue`)**: All handlers receive every message.
- **Load Balancing (`LBQueue`)**: Messages are distributed to one available worker.

```rust
#[trigger(LBQueue(WorkerQueue))]
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

## 4. Event Traceability (Publishing)

Starting from v0.1.0, any event published within a service can be traced throughout the entire system.

### Using `publish()`
Wrap your event production logic in `service_daemon::publish()` to capture the current `InstanceId` and generate a unique `MessageId`.

```rust
use service_daemon::publish;

#[service]
async fn my_service() -> anyhow::Result<()> {
    while !service_daemon::is_shutdown() {
        // Traceable event publishing
        publish("my_event", || async {
            MyProvider::notify().await;
        }).await;

        service_daemon::sleep(Duration::from_secs(10)).await;
    }
    Ok(())
}
```

### Traceability Benefits
- **Source Attribution**: See exactly which service instance fired a signal.
- **Message IDs**: Correlation of logs across multiple trigger handlers.
- **Debug Visibility**: High-priority diagnostics via `DaemonLayer`.

## 5. Resilience: Automatic Handler Retries

Starting from v0.1.0, individual trigger handler failures (returning `Err`) are automatically retried using the same global **Exponential Backoff** policy as regular services.

### How it works
When a handler fails:
1. The framework creates a `TriggerInvocation` "mini-host" for that specific event.
2. It uses a `BackoffController` to calculate the next wait time.
3. The payload is shared via `Arc` internally — retries **never** deep-copy business data.
4. Retries continue until the handler succeeds or the system shuts down.

### Payload Handling

The framework wraps every payload in `Arc<P>` at the dispatch boundary. How the payload reaches your handler depends on your function signature:

| Handler Signature | What Happens | `Clone` Required? |
|:---|:---|:---|
| `async fn handler(data: T)` | Macro auto-clones from `Arc` | **Yes** |
| `async fn handler(data: Arc<T>)` | Zero-copy pointer pass | **No** |

> [!TIP]
> For large payloads or types that cannot implement `Clone`, declare your handler parameter as `Arc<T>`. This gives you true zero-copy access and works with any type.

---

## 6. More Information

- [Provider Best Practices](provider-best-practices.md): Deep dive into defining custom providers.
- [Concept Clarification (FAQ)](pitfalls-faq.md#2-lifecycle--paradigms): Understanding the difference between managed triggers and standard services.

[Back to README](../../README.md)
