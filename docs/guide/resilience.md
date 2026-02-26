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

let daemon = ServiceDaemon::builder()
    .with_restart_policy(policy)
    .build();
daemon.run().await?
```

### 1.1. The `BackoffController` Abstraction

Internal to the framework, retry logic is managed by the **`BackoffController`**. This stateful component tracks:
- Current retry delay.
- Consecutive failure count.
- Interruption-aware waiting (respecting shutdown/reload signals during the sleep period).

Because this controller is now a shared abstraction, **Trigger Handlers** also benefit from identical exponential backoff and jitter behavior when they encounter errors.

### 1.2. Immediate Restart on Reload Signal
Even if a service is in a restart delay period (e.g. after a failure), the `ServiceDaemon` remains reactive. If a **Reload Signal** is received (typically due to a dependency update), the daemon will interrupt the delay and restart the service immediately with the new configuration.

### 1.2. Fatal Errors
Sometimes a service encounters an error that it cannot recover from via a restart (e.g., a missing required environment variable or an invalid license). In such cases, the service can return `ServiceError::Fatal`.

When a service returns a `Fatal` error, the `ServiceDaemon` will **permanently stop** that service and transition its status to `Terminated`, bypassing the restart policy entirely.

```rust
use service_daemon::models::ServiceError;

#[service]
async fn license_checker() -> anyhow::Result<()> {
    if !check_license().await {
        return Err(ServiceError::Fatal("Invalid license key".into()).into());
    }
    // ...
    Ok(())
}
```

## 2. Advanced Resilience: Wave Timeouts

The `RestartPolicy` also controls how long the daemon waits for services during startup and shutdown waves.

```rust
let policy = RestartPolicy::builder()
    .wave_spawn_timeout(Duration::from_secs(10)) // Wait up to 10s for Healthy status
    .wave_stop_timeout(Duration::from_secs(45))  // Wait up to 45s for graceful stop
    .build();
```

- **Spawn Timeout**: The maximum time a startup wave waits for all services within it to report `Healthy`.
- **Stop Timeout**: The maximum time a shutdown wave waits for all services within it to exit gracefully before forcing an abort.

## 3. Managing CPU-Intensive & Blocking Tasks

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
2. **Error Suppression**: If a service exits with an error *after* the shutdown signal has been sent, the daemon treats it as a successful exit. This prevents irrelevant error logs (e.g., "channel closed" or "network unreachable") that naturally occur during the teardown of dependencies.
3. **Grace Period**: The daemon waits for a grace period (default: 30s) per wave.
4. **Forced Abort**: Services that don't exit within the period are aborted.

[Back to README](../../README.md)
