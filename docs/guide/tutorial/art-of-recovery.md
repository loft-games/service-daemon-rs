# The Art of Recovery

Systems fail. Networks drop, databases crash, and someone might change a configuration value while your service is running. 

In `service-daemon-rs`, we don't just "let it crash". We provide a mechanism for **Graceful Migration** and **State Recovery**.

---

## 1. The "Stateful" Service

To handle lifecycle events, we use `state()` instead of `is_shutdown()`. This allows our service to respond to different phases of its life.

```rust
use service_daemon::{service, state, ServiceStatus};

#[service]
pub async fn robust_service() -> anyhow::Result<()> {
    // Match the current lifecycle state
    match state() {
        ServiceStatus::Initializing => {
            tracing::info!("First time starting up!");
        }
        ServiceStatus::Restoring => {
            tracing::info!("Wait, I was here before! Let me recover my state...");
        }
        ServiceStatus::Recovering(err) => {
            tracing::warn!("I crashed last time with error: {}. Fixing myself...", err);
        }
        _ => {}
    }
    
    // Your main loop logic...
    Ok(())
}
```

## 2. The "Shelf": Protecting your data

If a service needs to restart (e.g., because a dependency updated), it will be destroyed and re-created. How do you keep your progress? You put it on the **Shelf**.

```rust
use service_daemon::prelude::*; // shelve and unshelve are here!

#[service]
pub async fn counter_service() -> anyhow::Result<()> {
    // 1. Try to get our previous count from the shelf
    let mut count: u32 = unshelve("my_counter").await.unwrap_or(0);
    
    while !is_shutdown() {
        count += 1;
        tracing::info!("Count is: {}", count);
        
        // 2. If someone tells us we need to reload or shut down...
        if matches!(state(), ServiceStatus::NeedReload | ServiceStatus::ShuttingDown) {
            // ...save our progress to the shelf!
            shelve("my_counter", count).await;
            break;
        }
        
        service_daemon::sleep(Duration::from_secs(1)).await;
    }
    
    // 3. Mark the lifecycle transition as complete
    done();
    Ok(())
}
```

## 3. Why this matters? (The Provider Update)

Imagine `counter_service` depends on an `ApiKey`. If the `ApiKey` is updated in another service, the framework will:
1.  Signal `counter_service` that it **NeedsReload**.
2.  The service sees this, **shelves** its count, and exits.
3.  The framework instantly restarts the service with the **new** `ApiKey`.
4.  The new instance **unshelves** the count and continues from where it left off.

To the user, it looks like a seamless update. To the developer, it’s a clean state migration.

---

[**← Previous Step: Reactive Triggers**](reactive-triggers.md) | [**Next Step: DIY Providers →**](diy-providers.md)
