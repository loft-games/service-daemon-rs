# Intelligent State Management

`service-daemon-rs` optimizes shared state synchronization based on how your services declare their dependencies.

## 1. Snapshots & Mutability Patterns

### The Snapshot Pattern (Read-Only)
Declare a dependency as `Arc<T>` to get a consistent, read-only snapshot.
- **Zero Lockdown**: Readers never block, even during writes.
- **High Performance**: Identical to a raw pointer path if no mutation is active.

### The Mutability Pattern (Zero-Copy CoW)
Declare a dependency as `Arc<RwLock<T>>` or `Arc<Mutex<T>>` to gain write access.
- **Automatic Promotion**: The system upgrades the provider to a "Managed State" if any lock is requested.
- **Zero-Copy Publishing**: When a writer commits, a new `Arc<T>` is published.

```rust
#[service]
pub async fn writer_service(stats: Arc<RwLock<GlobalStats>>) -> anyhow::Result<()> {
    let mut guard = stats.write().await;
    guard.total_processed += 1;
    Ok(()) // Changes published and Watch triggers fired on Drop
}
```

## 2. Unified Status Plane

The Status Plane provides services with lifecycle awareness via the `ServiceStatus` enum.

| Status | Description |
|--------|-------------|
| `Initializing` | Fresh start |
| `Restoring` | Warm start with shelved data |
| `Recovering(err)`| Crash recovery with error context |
| `Healthy` | Normal operation |
| `NeedReload` | Dependency changed, save state now |
| `ShuttingDown` | Shutdown in progress |
| `Terminated` | Service has exited and is ready for collection |

### Lifecycle Utilities
- `state()`: Get current status.
- `done()`: Signal initialization complete (prevents wave hangs).
- `is_shutdown()`: Check if service should stop.
- `sleep(duration)`: Interruptible async sleep.

## 3. State Handoff (Shelving)

Persist small amounts of state across service restarts or reloads:

```rust
use service_daemon::{shelve, unshelve};

#[service]
async fn persistence_service() -> anyhow::Result<()> {
    // 1. Restore
    let history: Option<Vec<String>> = unshelve("history").await;
    
    // 2. Work...
    
    // 3. Save for next generation
    shelve("history", vec!["event".into()]).await;
    Ok(())
}
```

[Back to README](../../README.md)
