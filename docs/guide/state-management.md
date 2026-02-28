# State Management

Effective state management is key to building reactive applications. This guide covers how to manage shared state and persistent service data.

## 0. Quick Start: The Heartbeat Pattern

Every service in `service-daemon-rs` revolves around **Providers** (Data) and **Services** (Logic).

```rust
use service_daemon::{provider, service, sleep, is_shutdown};
use std::sync::Arc;
use std::time::Duration;

// 1. Define a Provider (Type-Safe Global State)
#[provider(5)]
pub struct HeartbeatInterval(pub u64);

// 2. Define a Service (The Business Logic)
#[service]
pub async fn heartbeat_service(interval: Arc<HeartbeatInterval>) -> anyhow::Result<()> {
    while !is_shutdown() {
        tracing::info!("Lub-dub...");
        sleep(Duration::from_secs(interval.0)).await;
    }
    Ok(())
}
```

> [!TIP]
> **Proactive Lifecycle**: `service_daemon::sleep()` is cancellation-aware. It wakes up immediately if a shutdown signal is detected, ensuring your app stops gracefully without hanging.

---

> [!TIP]
> Unsure whether to use a Provider (State) or the Shelf? See the [State vs. Shelf comparison in the FAQ](pitfalls-faq.md#3-providers--state).

`service-daemon-rs` optimizes shared state synchronization based on how your services declare their dependencies.

## 1. Snapshots & Mutability Patterns

`StateManager` manages the transition between immutable singletons and mutable tracked state. It provides a **"Macro Illusion"** allowing services to interact with state via standard `RwLock` or `Mutex` interfaces, while internally managing snapshots for the reactive `Watch` system.

### The Mutability Pattern (Zero-Copy CoW)
Declare a dependency as `Arc<RwLock<T>>` or `Arc<Mutex<T>>` to gain write access.
- **Automatic Promotion**: The system seamlessly upgrades the provider to a `TrackedRwLock` upon the first lock request.
- **Zero-Copy Publishing**: Use `guard.publish(Arc<T>)` to replace the entire state with a new pointer. This is the **highest performance path** for large types.

> [!NOTE]
> **Internal Mechanics**: For details on how `StateManager` manages transitions using `OnceCell` and `tokio::sync::watch`, see [Internal Architecture: State Management](../architecture/internal-overview.md#7-coremanaged_staters).

```rust
#[service]
pub async fn stats_updater(stats: Arc<RwLock<GlobalStats>>) -> anyhow::Result<()> {
    let mut guard = stats.write().await;
    
    // Path A: In-place mutation (requires T: Clone internally)
    guard.total_processed += 1;
    guard.commit(); 
    
    // Path B: Full replacement (Zero-Copy)
    let new_stats = Arc::new(compute_diff(&*guard));
    guard.publish(new_stats); 
    
    Ok(()) // Final commit happens on Drop if not manually called
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

### Standard Handoff (Destructive)
Use `unshelve()` when you only need to read the data once (e.g., at startup).
```rust
let history: Option<Vec<String>> = unshelve("history").await;
```

### Persistent Read (Non-Destructive)
Use `shelve_clone()` when you need to read the same data repeatedly without removing it from the shelf (e.g., inside a trigger's `handle_step`).
```rust
use service_daemon::shelve_clone;
let config: Option<AppConfig> = shelve_clone("app_config").await;
```

---


[Back to README](../../README.md)
