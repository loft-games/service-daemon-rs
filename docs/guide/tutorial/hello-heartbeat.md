# Hello, Heartbeat!

Every great journey starts with a single beat. In this first step, we’ll build a simple "Heartbeat" service that prints a message to the console every few seconds. 

This simple example introduces the three pillars of the framework: **Providers**, **Services**, and the **Daemon**.

---

## 1. The Provider (The "What")

In `service-daemon-rs`, we don't pass configuration strings or raw integers around. We use **Providers**. A provider is just a type that the framework knows how to "provide" to your services.

```rust
use service_daemon::provider;

/// A simple configuration for our heartbeat interval.
#[provider(default = 5)]
pub struct HeartbeatInterval(pub u64);
```

> [!NOTE]
> By using `#[provider]`, the framework automatically handles the lifecycle of this value. You don't need to wrap it in a `Mutex` or an `Arc` manually; the framework does it for you. It's essentially "Type-Safe Global State".

## 2. The Service (The "How")

Now, let's write the logic. A service is an `async fn` marked with `#[service]`. It can "ask" for any provider by simply adding it to its arguments as an `Arc<T>`.

```rust
use service_daemon::{service, sleep, is_shutdown};
use std::sync::Arc;
use std::time::Duration;

#[service]
pub async fn heartbeat_service(interval: Arc<HeartbeatInterval>) -> anyhow::Result<()> {
    tracing::info!("Heartbeat service started with interval: {}s", interval.0);

    // The core loop: we run until the daemon tells us to stop.
    while !is_shutdown() {
        tracing::info!("Lub-dub...");
        
        // Use the framework's sleep helper—it's interruptible!
        sleep(Duration::from_secs(interval.0)).await;
    }
    
    tracing::info!("Heartbeat service stopped gracefully.");
    Ok(())
}
```

### Pro Tip: Why `tracing`?
You’ll notice we use `tracing::info!` instead of `println!`. In `service-daemon-rs`, logs are handled by a dedicated, non-blocking **LogService**. This ensures that printing to the console never slows down your service's real work.

### Why `is_shutdown()` and `sleep()`?
*   `is_shutdown()`: Returns `true` when the user presses Ctrl+C or the system is stopping. This is a non-blocking check, perfect for loop conditions.
*   `service_daemon::sleep()`: Unlike standard `tokio::time::sleep`, this version is **cancellation-aware**. It wakes up immediately if a shutdown signal is detected. No more waiting for a long sleep to finish during shutdown!

## 3. The Grand Finale: running the Daemon

Finally, you just need to tell the framework to run. It will find all your `#[service]` functions automatically and start them in the background.

```rust
use service_daemon::ServiceDaemon;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // build() find all services, run() starts the engine!
    ServiceDaemon::builder().build().run().await
}
```

---

## What happened here?

1.  **Zero Config**: You didn't have to manually register `heartbeat_service`. The macro did it for you.
2.  **Type-Safe DI**: You didn't cast objects or look up strings. You asked for `Arc<HeartbeatInterval>`, and you got it.
3.  **Graceful by Default**: If you hit `Ctrl+C`, the service exits immediately thanks to `is_shutdown()` and `sleep()`.
4.  **Sync-Trap**: If you accidentally write synchronous, blocking code in an `async fn` service, the framework will detect it and issue a `tracing::warn!` to keep your system responsive.

---

[**Next Step: Reactive Triggers →**](reactive-triggers.md)
