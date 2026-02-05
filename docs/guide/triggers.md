# Event Triggers

Triggers are specialized services with built-in event loops that execute your functions when specific events occur.

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
#[trigger(template = Cron, target = CleanupSchedule)]
async fn hourly_cleanup() -> anyhow::Result<()> {
    tracing::info!("Cleaning up...");
    Ok(())
}
```

### Queue Triggers
- **Broadcast (`Queue`)**: All handlers receive every message.
- **Load Balancing (`LBQueue`)**: Messages are distributed to one available worker.

```rust
#[trigger(template = LBQueue, target = WorkerQueue)]
async fn worker(item: Task) -> anyhow::Result<()> { ... }
```

### Watch Trigger (State Change)
Executes automatically whenever shared state (`Arc<RwLock<T>>` or `Arc<Mutex<T>>`) is modified. Internally, this leverages the `ServiceDaemon`'s reload mechanism: the service is re-spawned with a fresh snapshot exactly when the state changes.

```rust
#[trigger(template = Watch, target = MyData)]
pub async fn on_data_changed(snapshot: Arc<MyData>) -> anyhow::Result<()> {
    tracing::info!("New value: {}", snapshot.value);
    Ok(())
}
```

## 3. Parameter Mapping Rules

1. **Implicit Payload**: The first parameter that is *not* an `Arc<T>` is treated as the event payload.
2. **Explicit Payload**: Any parameter marked with `#[payload]` is the payload (allows `Arc<Payload>`).
3. **DI Resources**: All other `Arc<T>` parameters are resolved via the DI system.

[Back to README](../../README.md)
