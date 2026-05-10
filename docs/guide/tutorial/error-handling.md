# Error Handling & Retries

A background daemon's primary job is to keep your services alive. But "alive" doesn't mean "restarting in a tight loop forever".

In this chapter, we'll learn the normal user-facing controls for retries, throughput, fatal errors, and startup wave timeouts.

---

## 1. Tuning the Restart Policy

By default, `ServiceDaemon` uses exponential backoff. You can customize this globally to match your environment.

```rust,ignore
use service_daemon::{RestartPolicy, ServiceDaemon};
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let policy = RestartPolicy::builder()
        .initial_delay(Duration::from_secs(2)) // Start with 2s delay
        .multiplier(1.5)                       // Increase wait by 50% each time
        .max_delay(Duration::from_secs(300))   // Cap at 5 minutes
        .jitter_factor(0.1)                    // Add 10% randomness
        .build();

    let mut daemon = ServiceDaemon::builder()
        .with_restart_policy(policy)
        .build();

    daemon.run().await;
    daemon.wait().await?;

    Ok(())
}
```

### What restarts?

- If a service returns a normal `Err`, the daemon restarts it with backoff.
- If a service panics, the daemon treats it as recoverable and restarts it with backoff.
- If a service returns `Ok(())`, the daemon starts a fresh generation immediately.
- If a service returns `ServiceError::Fatal`, the daemon stops that service permanently.

The daemon also protects itself from pathological tight restart loops, but most applications only need to tune `RestartPolicy`.

### Trigger handler retries

The same restart policy also controls individual trigger handler retries. If a handler returns `Err`, the framework retries that event with backoff.

For trigger handlers that should not retry forever, set `trigger_max_retries` on `RestartPolicy`. Do not use `trigger_max_retries` to control service lifecycle; services should return `ServiceError::Fatal` when they need to stop permanently.

---

## 2. Throughput: Scaling Policy

`RestartPolicy` controls *time* -- delays and retries between failures. `ScalingPolicy` controls *volume* -- how many trigger handlers may run concurrently and when concurrency is allowed to grow under load.

The default limit for streaming triggers like `Queue` is **64** concurrent handlers. If your system has high throughput requirements, tune it with the builder:

```rust,ignore
use service_daemon::{ScalingPolicy, ServiceDaemon};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let scaling = ScalingPolicy::builder()
        .initial_concurrency(4)    // Start with 4 slots
        .max_concurrency(128)      // Scale up to 128
        .scale_threshold(3)        // Scale up earlier under pressure
        .build();

    let mut daemon = ServiceDaemon::builder()
        .with_trigger_config(scaling)
        .build();

    daemon.run().await;
    daemon.wait().await?;

    Ok(())
}
```

> [!TIP]
> For triggers like `Cron`, `Watch`, or `Notify`, scaling is automatically disabled. The framework does not start the background scale monitor for trigger types that do not need it.

---

## 3. Fatal Errors: The Kill Switch

Sometimes, a service encounters an error that **cannot** be fixed by a restart. For example:

- A missing mandatory environment variable.
- An invalid license key.
- Incompatible hardware version.

In these cases, return `ServiceError::Fatal`.

```rust,ignore
use service_daemon::ServiceError;

#[service]
async fn license_watcher() -> anyhow::Result<()> {
    if !verify_license().await {
        return Err(ServiceError::Fatal("License expired".into()).into());
    }

    Ok(())
}
```

When a `Fatal` error occurs during service execution, the daemon transitions that service to `Terminated` and stops trying. The rest of the system keeps running normally.

## 4. Wave Timeouts

The restart policy also controls how long the daemon waits for startup and shutdown waves.

- `wave_spawn_timeout`: Maximum time to wait for services in a wave to become `Healthy`. If this limit is reached, the daemon logs a warning and proceeds to the next wave. The slow services continue starting in the background.
- `wave_stop_timeout`: Maximum time to wait for services in a wave to exit during shutdown.

```rust,ignore
let policy = RestartPolicy::builder()
    .wave_spawn_timeout(Duration::from_secs(10))
    .wave_stop_timeout(Duration::from_secs(45))
    .build();
```

---

[**<- Previous Step: DIY Providers**](./diy-providers.md) | [**Next Step: Priorities & Scheduling ->**](./priority-orchestration.md)
