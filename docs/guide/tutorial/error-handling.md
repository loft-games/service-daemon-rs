# Error Handling & Retries

A background daemon's primary job is to keep your services alive. But "alive" doesn't mean "restarting in a tight loop forever". 

In this chapter, we'll learn how to tune the engine's error handling and retry logic.

---

## 1. Tuning the Restart Policy

By default, `ServiceDaemon` uses exponential backoff. You can customize this globally to match your environment.

```rust,ignore
use service_daemon::{ServiceDaemon, RestartPolicy};
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let policy = RestartPolicy::builder()
        .initial_delay(Duration::from_secs(2)) // Start with 2s delay
        .multiplier(1.5)                       // Increase wait by 50% each time
        .max_delay(Duration::from_secs(300))   // Cap at 5 minutes
        .jitter_factor(0.1)                    // Add 10% randomness to prevent "thundering herd"
        .build();

    let mut daemon = ServiceDaemon::builder()
        .with_restart_policy(policy)
        .build();

    daemon.run().await;
    daemon.wait().await?;

    Ok(())
}
```

### 1.1. Services: Internal Restart-Storm Protection
Services retry indefinitely for recoverable errors, panics, and isolated startup failures. In addition to the configured backoff, supervisors apply an internal restart-storm guard: if repeated backoff-eligible failures happen inside a short window, the daemon may extend the effective restart delay.

This guard is not a public `RestartPolicy` setting yet. Clean `Ok(())` exits still restart immediately, `ServiceError::Fatal` still stops the service, and reload/shutdown signals still interrupt restart waits.

### 1.2. Shared for Triggers
These same restart policies apply to individual **Trigger Handlers**. If a handler returns `Err`, the framework will back off and retry the specific event according to trigger retry configuration.

For trigger handlers that should not retry forever, set `trigger_max_retries` on `RestartPolicy`. Do not use `trigger_max_retries` to control service lifecycle; services should return `ServiceError::Fatal` when they need to stop permanently.

---

## 2. Throughput: Scaling Policy

`RestartPolicy` controls *time* (delays and retries between failures). `ScalingPolicy` controls *volume* -- how many trigger handlers may run concurrently and when concurrency is allowed to grow under load.

The default limit for streaming triggers (like `Queue`) is **64** concurrent handlers. If your system has high throughput requirements, you can tune this:

```rust,ignore
use service_daemon::{ServiceDaemon, ScalingPolicy};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let scaling = ScalingPolicy::builder()
        .initial_concurrency(4)    // Start with 4 slots
        .max_concurrency(128)      // Scale up to 128
        .scale_threshold(3)        // Aggressive scaling: scale up earlier
        .build();

    let mut daemon = ServiceDaemon::builder()
        .with_trigger_config(scaling) // Register for all triggers
        .build();

    daemon.run().await;
    // ...
    Ok(())
}
```

> [!TIP]
> **Zero Overhead**: For triggers like `Cron` or `Notify`, scaling is automatically disabled. The framework will never start the background scale monitor or create a semaphore for these types unless they explicitly declare a need for it.

---

## 3. Fatal Errors: The Kill Switch

Sometimes, a service encounters an error that **cannot** be fixed by a restart. For example:
*   A missing mandatory environment variable.
*   An invalid license key.
*   Incompatible hardware version.

In these cases, you should use `ServiceError::Fatal`.

```rust,ignore
use service_daemon::ServiceError;

#[service]
async fn license_watcher() -> anyhow::Result<()> {
    if !verify_license().await {
        // This will tell the daemon: "Don't try to restart me again!"
        return Err(ServiceError::Fatal("License expired".into()).into());
    }
    
    // ... normal logic ...
    Ok(())
}
```

When a `Fatal` error occurs during service execution, the daemon transitions that service to `Terminated` and stops trying. The rest of the system keeps running normally.

Recoverable service errors and panics are different: they transition the service to `Recovering(...)` and restart with backoff plus the internal storm guard.

For `ServiceScheduling::Isolated`, startup allocation failures are also recoverable. If the daemon cannot create the isolated thread, private Tokio runtime, or startup bridge for one generation, it records an isolated startup failure and retries through the same recovery path.

For lazy providers, a `ProviderError::Fatal` raised during runtime initialization is treated differently: the service runner treats it as a daemon-wide shutdown signal and stops the `ServiceDaemon` cleanly.

## 4. Wave Timeouts

The `RestartPolicy` also controls how long the daemon waits for your services to report they are "Healthy" or to "Stop".

*   `wave_spawn_timeout`: The maximum time to wait for services in a wave to become `Healthy`. If this limit is reached, the daemon logs a warning and **proceeds to the next wave anyway**. The services continue their startup in the background.
*   `wave_stop_timeout`: Maximum time to wait for a service to exit before forcefully killing it.

> [!NOTE]
> **Deep Dive**: To understand the internal watchdog mechanism and the mathematical models behind our restart policies, see the [Resilience & Monitoring](../resilience.md) design document.

---

[**<- Previous Step: DIY Providers**](./diy-providers.md) | [**Next Step: Priorities & Scheduling ->**](./priority-orchestration.md)
