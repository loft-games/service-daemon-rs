# Unit Testing & Simulation

Testing background services requires control over lifecycle, injected state, and runtime changes.

The `simulation` feature gives tests a controlled daemon sandbox. This chapter focuses on the common testing pattern: start only the services you care about, pre-fill state, mutate the sandbox, and assert the result.

---

## 1. Enabling the Sandbox

Simulation is a feature-gated toolbox. In your `Cargo.toml`, enable it for tests:

```toml
[dev-dependencies]
service_daemon = { version = "...", features = ["simulation"] }
```

## 2. Using `MockContext`

In a simulation test, you run a fully functional but isolated daemon. Instead of letting every auto-registered service run, tag the service under test and select only that tag.

```rust,ignore
use service_daemon::prelude::*;
use std::time::Duration;

#[service(tags = ["sim_shelf"])]
async fn shelf_reader_service() -> anyhow::Result<()> {
    loop {
        match state() {
            ServiceStatus::Initializing | ServiceStatus::Restoring => {
                if let Some(val) = unshelve::<String>("config_key").await {
                    shelve("read_result", val).await;
                }
                done();
            }
            ServiceStatus::Healthy => {
                if let Some(val) = unshelve::<String>("dynamic_key").await {
                    shelve("dynamic_result", val).await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use service_daemon::{MockContext, Registry};

    #[tokio::test]
    async fn simulation_can_seed_and_mutate_shelf() -> anyhow::Result<()> {
        let registry = Registry::builder().with_tag("sim_shelf").build();
        let shelf_reader_id = registry
            .services()
            .iter()
            .find(|service| service.name() == "shelf_reader_service")
            .and_then(|service| service.instances().first().copied())
            .map(|instance| instance.instance_id())
            .expect("shelf_reader_service should be selected");
        let (builder, handle) = MockContext::builder()
            .with_shelf::<String>(shelf_reader_id, "config_key", "initial_val".into())
            .build();

        let daemon = builder.with_registry(registry).build();

        let cancel = daemon.cancel_token();
        let daemon_task = tokio::spawn(async move {
            let mut daemon = daemon;
            daemon.run().await;
            if let Err(err) = daemon.wait().await {
                panic!("daemon.wait() failed: {err}");
            }
        });

        tokio::time::sleep(Duration::from_millis(200)).await;
        let result: Option<String> = handle.get_shelf(shelf_reader_id, "read_result");
        assert_eq!(result, Some("initial_val".into()));

        handle.set_shelf::<String>(shelf_reader_id, "dynamic_key", "mid_flight_val".into());

        tokio::time::sleep(Duration::from_millis(200)).await;
        let result: Option<String> = handle.get_shelf(shelf_reader_id, "dynamic_result");
        assert_eq!(result, Some("mid_flight_val".into()));

        cancel.cancel();
        let _ = daemon_task.await;
        Ok(())
    }
}
```

## 3. The `SimulationHandle`

The `SimulationHandle` lets tests inspect and mutate the running sandbox without reaching into daemon internals.

### Snapshot Inspection

```rust,ignore
let service_instance_id = registry.services()[0].instance_id;
let val: Option<String> = handle.get_shelf(service_instance_id, "key");
let status = handle.get_status(service_instance_id);

if handle.has_shelf(service_instance_id, "key") {
    // assert or trigger the next test step
}
```

### Mutation API

```rust,ignore
handle.set_status(service_instance_id, ServiceStatus::NeedReload);
handle.set_shelf::<String>(service_instance_id, "config_override", "NEW_VALUE".into());
```

## 4. Supported test controls

- **Pre-populate the Shelf**: Test state recovery without waiting for a real crash.
- **Dynamic Injection**: Overwrite shelf values while the daemon is running.
- **Status Flipping**: Force services into `NeedReload`, `Recovering`, or `ShuttingDown` to test their reaction logic.

---

[**<- Previous Step: Priorities & Scheduling**](./priority-orchestration.md) | [**Back to Quick Start Guide ->**](./quick-start.md)
