# Playing God: Simulator

Testing background services is notoriously difficult. How do you test what happens when a database fails mid-flight? Or how your service reacts when its configuration is swapped at 2 AM?

The `simulation` feature (God-mode) gives you total control over the environment.

---

## 1. Enabling the Sandbox

Simulation is a feature-gated toolbox. In your `Cargo.toml`, make sure you enable it for your tests:

```toml
[dev-dependencies]
service_daemon = { version = "...", features = ["simulation"] }
```

## 2. Using `MockContext`

In a simulation test, you run a **fully functional but isolated Daemon**. Instead of global auto-discovery, you use a `MockContext` to inject controlled resources and specific services into a sandbox.

```rust
use service_daemon::prelude::*;
use std::time::Duration;

// --- 1. The Robust Tested Service ---
#[service(tags = ["sim_shelf"])]
async fn shelf_reader_service() -> anyhow::Result<()> {
    loop {
        match state() {
            ServiceStatus::Initializing | ServiceStatus::Restoring => {
                // Phase 1: Try to read data pre-filled by the MockContext
                if let Some(val) = unshelve::<String>("config_key").await {
                    shelve("read_result", val).await;
                }
                done();
            }
            ServiceStatus::Healthy => {
                // Phase 2: React to dynamic/mid-flight injection
                if let Some(val) = unshelve::<String>("dynamic_key").await {
                    shelve("dynamic_result", val).await;
                    // In a real test, we might stop or continue working
                }

                if !sleep(Duration::from_millis(100)).await {
                    continue;
                }
            }
            ServiceStatus::ShuttingDown => break,
            _ => break,
        }
    }
    Ok(())
}

// --- 2. The God's Eye Test Suite ---
#[cfg(test)]
mod tests {
    use super::*;
    use service_daemon::{MockContext, Registry};

    #[tokio::test]
    async fn test_two_phase_simulation() -> anyhow::Result<()> {
        // Phase 1: Pre-fill some initial state (Sandbox setup)
        let (builder, handle) = MockContext::builder()
            .with_shelf::<String>("shelf_reader_service", "config_key", "initial_val".into())
            .build();

        let daemon = builder
            .with_registry(Registry::builder().with_tag("sim_shelf").build())
            .build();

        // Start the daemon in the background
        let cancel = daemon.cancel_token();
        let daemon_task = tokio::spawn(async move {
            let mut daemon = daemon;
            daemon.run().await.unwrap();
            daemon.wait().await.unwrap();
        });

        // Verify Phase 1: service initialized and read pre-filled value
        tokio::time::sleep(Duration::from_millis(200)).await;
        
        // Recommended: Use the safe, lock-free get_shelf() API
        let result: Option<String> = handle.get_shelf("shelf_reader_service", "read_result");
        assert_eq!(result, Some("initial_val".into()));

        // Phase 2: Mid-flight Intervention (The God Hand)
        handle.set_shelf::<String>("shelf_reader_service", "dynamic_key", "mid_flight_val".into());

        // Verify Phase 2: service observed the dynamic mutation
        tokio::time::sleep(Duration::from_millis(200)).await;
        
        // Safe access across .await points
        let result: Option<String> = handle.get_shelf("shelf_reader_service", "dynamic_result");
        assert_eq!(result, Some("mid_flight_val".into()));

        cancel.cancel();
        let _ = daemon_task.await;
        Ok(())
    }
}
```

## 3. The "God Hand" (`SimulationHandle`)

The `SimulationHandle` allows you to reach into the running sandbox and change things while the services are active. It provides a **Safe Read API** specifically designed to prevent deadlocks in tests.

### Safe Accessors (Recommended)
These methods acquire and release internal locks instantly, making them safe to use even if you `await` other things later in your test.

```rust
// Inspect shelf values without holding locks
let val: Option<String> = handle.get_shelf("svc", "key");

// Inspect service status safely
let status = handle.get_status(svc_id);

// Check if a key exists
if handle.has_shelf("svc", "key") { ... }
```

### Mutation API
```rust
// Mid-test: Change the status of a service to force a reload
handle.set_status(service_id, ServiceStatus::NeedReload);

// Mid-test: Inject a new value into the shelf
handle.set_shelf::<String>("target_svc", "config_override", "NEW_VALUE".into());
```

## 4. WARNING: Deadlock Risk

Directly accessing `handle.resources()` and holding a `DashMap::Ref` across an `.await` point will cause a **deadlock**. 

**Why?** If your test holds a read lock on a shelf while you `await` the daemon tasks, and one of those services tries to call `shelve()` (which needs a write lock), the system will hang forever.

**Always use `get_shelf()` and `get_status()`** instead of digging into `resources()`.

## 5. Summary of Powers

*   **Pre-populate the Shelf**: Test state recovery without waiting for a real crash.
*   **Dynamic Injection**: Overwrite dependencies at runtime.
*   **Status Flipping**: Force services into `NeedReload`, `Recovering`, or `ShuttingDown` to test their reaction logic.

---

[**-- Previous Step: Waves of Orchestration**](orchestration-waves.md) | [**Next Step: Under the Hood --**](under-the-hood.md)
