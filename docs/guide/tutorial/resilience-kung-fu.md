# Resilience Kung-Fu

A background daemon's primary job is to keep your services alive. But "alive" doesn't mean "restarting in a tight loop forever". 

In this chapter, we'll learn how to tune the engine's resilience.

---

## 1. Tuning the Restart Policy

By default, `ServiceDaemon` uses exponential backoff. You can customize this globally to match your environment.

```rust
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

### 1.1. Shared for Triggers
Starting from v0.1.0, these same restart policies apply to individual **Trigger Handlers**. If a handler returns `Err`, the framework will back off and retry the specific event before giving up or shutting down.

---

## 2. Mastering Throughput: Scaling Policy

While `RestartPolicy` handles *time* (delays and retries), the **`ScalingPolicy`** handles *volume*. It determines how many trigger handlers can run concurrently and when to scale up.

The default limit for streaming triggers (like `Queue`) is **64** concurrent handlers. If your system has high throughput requirements, you can tune this:

```rust
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

Sometimes, a service encounter an error that **cannot** be fixed by a restart. For example:
*   A missing mandatory environment variable.
*   An invalid license key.
*   Incompatible hardware version.

In these cases, you should use `ServiceError::Fatal`.

```rust
use service_daemon::models::ServiceError;

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

When a `Fatal` error occurs, the daemon transitions that service to `Terminated` and stops trying. The rest of the system keeps running normally.

## 4. Wave Timeouts

The `RestartPolicy` also controls how long the daemon waits for your services to report they are "Healthy" or to "Stop".

*   `wave_spawn_timeout`: The maximum time to wait for services in a wave to become `Healthy`. If this limit is reached, the daemon logs a warning and **proceeds to the next wave anyway**. The services continue their startup in the background.
*   `wave_stop_timeout`: Maximum time to wait for a service to exit before forcefully killing it.

> [!NOTE]
> **Deep Dive**: To understand the internal watchdog mechanism and the mathematical models behind our restart policies, see the [Resilience & Monitoring](../resilience.md) design document.

---

[**<- Previous Step: DIY Providers**](diy-providers.md) | [**Next Step: Waves of Orchestration ->**](orchestration-waves.md)
