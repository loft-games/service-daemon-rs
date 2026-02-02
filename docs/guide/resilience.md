# Resilience & Lifecycle Management

This guide explains how `ServiceDaemon` ensures application stability through automatic restarts, priority-based orchestration, and graceful shutdown.

## 1. Automatic Restarts: Exponential Backoff & Jitter

Services that fail (return `Err`) are automatically restarted with exponential backoff and **randomized jitter** to prevent thundering herd issues.

```rust
use service_daemon::{ServiceDaemon, RestartPolicy};
use std::time::Duration;

let policy = RestartPolicy::builder()
    .initial_delay(Duration::from_secs(2))
    .max_delay(Duration::from_secs(300))
    .multiplier(1.5)
    .jitter_factor(0.1) // 10% randomization
    .build();

let daemon = ServiceDaemon::from_registry_with_policy(policy);
daemon.run().await?
```

## 2. Advanced Resilience: Managing CPU-Intensive & Blocking Tasks

The asynchronous executor (Tokio) relies on cooperative multitasking. If a service performs a long-running CPU computation or a blocking I/O operation without yielding, it will **stall the entire daemon**.

### CPU-Intensive Tasks
Use `tokio::task::spawn_blocking` to offload heavy calculations:

```rust
#[service]
async fn compute_service() -> anyhow::Result<()> {
    while !service_daemon::is_shutdown() {
        let result = tokio::task::spawn_blocking(|| {
            perform_heavy_calculation()
        }).await?;
        
        service_daemon::sleep(Duration::from_secs(1)).await;
    }
    Ok(())
}
```

### The `#[allow_sync]` Escape Hatch
If your function is synchronous but guaranteed to be fast and non-blocking (e.g., in-memory math), use `#[allow_sync]` to suppress runtime warnings.

> [!WARNING]
> **Never** use `#[allow_sync]` for network requests or disk I/O. This will cause severe performance degradation and may block shutdown.

## 3. Lifecycle Priorities

Services are assigned a `u8` priority (default 50) to determine their relative importance.
- **Startup**: Descending order (100 -> 0). Core systems start first.
- **Shutdown**: Ascending order (0 -> 100). Core systems stop last.

| Level (u8) | Constant | Purpose |
| :--- | :--- | :--- |
| **100** | `SYSTEM` | Core systems (Logging, Metrics). |
| **80** | `STORAGE` | Data providers, database pools. |
| **50** | `DEFAULT` | Core business logic and triggers. |
| **0** | `EXTERNAL` | API Gateways, HTTP servers. |

```rust
#[service(priority = ServicePriority::SYSTEM)]
pub async fn log_flush() { ... }
```

## 4. Graceful Shutdown

The daemon uses `CancellationToken` to signal services to stop. 
1. **Notification**: All services are notified via `is_shutdown()`.
2. **Grace Period**: The daemon waits for a grace period (default: 30s) per wave.
3. **Forced Abort**: Services that don't exit within the period are aborted.

[Back to README](../../README.md)
