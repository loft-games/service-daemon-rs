# State Management & Recovery

Systems fail. Networks drop, databases crash, and someone might change a configuration value while your service is running. 

In `service-daemon-rs`, we don't just "let it crash". We provide a mechanism for **Graceful Migration** and **State Recovery**.

---

## 1. The "Stateful" Service

To handle lifecycle events, we use `state()` instead of `is_shutdown()`. This allows our service to respond to different phases of its life.

```rust,ignore
use service_daemon::{done, service, sleep, state};
use service_daemon::models::ServiceStatus;
use std::time::Duration;

#[service]
pub async fn robust_service() -> anyhow::Result<()> {
    loop {
        match state() {
            ServiceStatus::Initializing => {
                tracing::info!("First time starting up!");
                done(); // Transition to Healthy
            }
            ServiceStatus::Restoring => {
                tracing::info!("Dependency changed, warm restart...");
                done();
            }
            ServiceStatus::Recovering(err) => {
                tracing::warn!("Crashed last time: {}. Recovering...", err);
                done();
            }
            ServiceStatus::Healthy => {
                tracing::info!("Working normally.");
                if !sleep(Duration::from_secs(5)).await {
                    continue; // Interrupted -- re-check state
                }
            }
            ServiceStatus::NeedReload => {
                tracing::info!("Performing cleanup before reload...");
                done(); // Acknowledge reload
                break;
            }
            ServiceStatus::ShuttingDown => {
                tracing::info!("Final cleanup before shutdown.");
                break;
            }
            _ => break,
        }
    }
    Ok(())
}
```

**Simulated Output:**
```text
INFO robust_service: First time starting up!
INFO robust_service: Working normally.
INFO robust_service: Working normally.
...
INFO robust_service: Performing cleanup before reload...
INFO robust_service: Final cleanup before shutdown.
```

## 2. The "Shelf": Protecting your data

If a service needs to restart (e.g., because a dependency updated), it will be destroyed and re-created. How do you keep your progress? You put it on the **Shelf**.

```rust,ignore
use service_daemon::prelude::*; // shelve, unshelve, done, state, sleep

#[service]
pub async fn counter_service() -> anyhow::Result<()> {
    let mut count = 0u32;

    loop {
        match state() {
            ServiceStatus::Initializing => {
                count = 0;
                tracing::info!("Starting fresh.");
                done();
            }
            ServiceStatus::Recovering(err) => {
                tracing::warn!("Recovering from crash: {}", err);
                count = unshelve::<u32>("my_counter").await.unwrap_or(0);
                done();
            }
            ServiceStatus::Restoring => {
                count = unshelve::<u32>("my_counter").await.unwrap_or(0);
                tracing::info!("Restored count: {}", count);
                done();
            }
            ServiceStatus::Healthy => {
                count += 1;
                tracing::info!("Count is: {}", count);

                // Persist state before potential failure
                shelve("my_counter", count).await;

                if !sleep(Duration::from_secs(1)).await {
                    continue;
                }
            }
            ServiceStatus::NeedReload => {
                tracing::info!("Shelving count ({}) before reload.", count);
                shelve("my_counter", count).await;
                done();
                break;
            }
            ServiceStatus::ShuttingDown => {
                tracing::info!("Final count: {}", count);
                break;
            }
            _ => break,
        }
    }
    Ok(())
}
```

**Simulated Output:**
```text
INFO counter_service: Starting fresh.
INFO counter_service: Count is: 1
INFO counter_service: Count is: 2
... (User updates a dependency) ...
INFO counter_service: Shelving count (2) before reload.
INFO counter_service: Restored count: 2
INFO counter_service: Count is: 3
```

To the user, it looks like an automatic update. To the developer, it's a clean state migration.

> [!NOTE]
> **Deep Dive**: To understand how the shelf handles type-erasure and thread-safety, check out the [State Management](docs/guide/state-management.md) design document.

## 3. Why this matters? (The Provider Update)

Imagine `counter_service` depends on an `ApiKey`. If the `ApiKey` is updated in another service, the framework will:
1.  Signal `counter_service` that it **NeedsReload**.
2.  The service sees this, **shelves** its count, and exits.
3.  The framework instantly restarts the service with the **new** `ApiKey`.
4.  The new instance **unshelves** the count and continues from where it left off.

## 4. Shared Mutable State & Warm Restarts

Sometimes you don't want to just "read" a provider; you want to **change** it. When you inject a provider as `Arc<RwLock<T>>` or `Arc<Mutex<T>>`, the framework promotes it to **Managed State**.

Crucially, when one service modifies a managed provider, every other service depending on it will undergo a **Warm Restart** (`ServiceStatus::Restoring`).

```rust,ignore
// 1. The Writer: Modifies shared stats
#[service]
pub async fn stats_writer(stats: Arc<RwLock<GlobalStats>>) -> anyhow::Result<()> {
    loop {
        match state() {
            ServiceStatus::Healthy => {
                {
                    let mut guard = stats.write().await;
                    guard.total_processed += 1;
                } // Lock dropped -> Triggers restarts for dependents!
                sleep(Duration::from_secs(5)).await;
            }
            ServiceStatus::Initializing | ServiceStatus::Restoring => done(),
            _ => break,
        }
    }
    Ok(())
}

// 2. The Reader: Automatically restarts when stats change
#[service]
pub async fn stats_reader(stats: Arc<GlobalStats>) -> anyhow::Result<()> {
    loop {
        match state() {
            ServiceStatus::Initializing => {
                tracing::info!("Started with stats: {}", stats.total_processed);
                done();
            }
            ServiceStatus::Restoring => {
                tracing::info!("Stats updated! Re-syncing logic...");
                done();
            }
            ServiceStatus::Healthy => {
                sleep(Duration::from_secs(1)).await;
            }
            _ => break,
        }
    }
    Ok(())
}
```

This pattern enables **Reactive Architecture**: your services automatically adapt to changing configurations or environmental data without manual orchestration.

---

[**<- Previous Step: Reactive Triggers**](docs/guide/tutorial/reactive-triggers.md) | [**Next Step: DIY Providers ->**](docs/guide/tutorial/diy-providers.md)
