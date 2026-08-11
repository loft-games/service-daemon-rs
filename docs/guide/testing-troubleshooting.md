# Testing & Troubleshooting

## 1. Common Patterns

### Resource Pooling
Use `#[provider]` for shared resources like database pools. This ensures that the resource is initialized once and injected safely into services.

```rust
#[provider]
pub async fn db_pool() -> MyDbPool {
    MyDbPool::connect("...").await.unwrap()
}
```

### Decoupled Communication
- Use **Queues** for fanning out tasks to multiple services.
- Use **Watch Triggers** to react to data changes without tight coupling.

## 2. Testing

The framework is designed for testability. Use `cargo test` to run the integrated suites.

### Integration Tests
Integration tests verify the full lifecycle of the daemon:
- Priority-based startup/shutdown order.
- Status transitions and shelving correctness.
- Signal propagation and trigger execution.

### Unit Testing with MockContext

Testing background services is difficult. The `simulation` feature gives tests controlled access to daemon state and reload signals.

Enable the feature in your `Cargo.toml`:
```toml
[dev-dependencies]
service-daemon = { path = "...", features = ["simulation"] }
```

Use `MockContext` to create an isolated sandbox and `SimulationHandle` to update the running daemon during the test:

```rust
#[service(tags = ["__test_sim__"])]
async fn my_service() -> anyhow::Result<()> {
    // service implementation ...
    Ok(())
}

#[tokio::test]
async fn test_two_phase_simulation() {
    let registry = Registry::builder().with_tag("__test_sim__").build();
    let service_instance_id = registry
        .services()
        .iter()
        .find(|service| service.name() == "my_service")
        .and_then(|service| service.instances().first().copied())
        .map(|instance| instance.instance_id())
        .expect("my_service should be selected");

    // 1. Set up the test shelf
    let simulation = MockContext::builder()
        .with_shelf::<String>(service_instance_id, "config_key", "initial_val".into())
        .with_registry(registry)
        .build();
    let cancel = simulation.cancel_token();
    
    // Start daemon in background
    let runner = simulation.clone();
    let daemon_task = tokio::spawn(async move {
        runner.run().await;
        runner.wait().await.ok();
    });

    // 2. Update state while the daemon is running
    simulation.set_shelf::<String>(service_instance_id, "dynamic_key", "new_val".into());

    // 3. Verify side-effects
    let result = simulation.get_shelf::<String>(service_instance_id, "processed_result");
    assert!(result.is_some());

    cancel.cancel();
    daemon_task.await.ok();
}
```

#### MockContext & SimulationHandle Capabilities

| Component | Method | Description |
| :--- | :--- | :--- |
| **Builder** | `with_shelf` | Pre-fills a Shelf entry for a specific service. |
| **Builder** | `with_status` | Pre-sets a lifecycle status in the isolated status plane. |
| **Builder** | `with_provider_override` | Installs a daemon-local provider value before eager initialization and service startup. |
| **Builder** | `with_registry` | Selects which registered services run inside the simulation. |
| **Handle** | `run` / `wait` / `shutdown` | Controls the sandbox daemon lifecycle. |
| **Handle** | `run_for_duration` | Runs the sandbox daemon for a bounded duration. |
| **Handle** | `get_shelf` | Reads a cloned value from the shelf without holding a lock after return. |
| **Handle** | `get_status` | Reads a cloned status value without holding a lock after return. |
| **Handle** | `set_shelf` | Writes or replaces a value in the shelf. |
| **Handle** | `set_status` | Updates a service's status and notifies waiters. |
| **Handle** | `trigger_reload` | Sends a reload signal for a service. |
| **Handle** | `override_provider` | Replaces a provider for this simulation daemon and reloads dependent generations through the provider watch path. |

Provider overrides are scoped to the simulation daemon. They do not write into the root provider slot and do not affect another simulation or production daemon in the same process.

```rust
#[derive(Clone)]
struct TestConfig {
    endpoint: String,
}

let simulation = MockContext::builder()
    .with_provider_override(TestConfig {
        endpoint: "memory://test".to_string(),
    })
    .with_registry(Registry::builder().with_tag("__test_sim__").build())
    .build();

simulation.run().await;

simulation.override_provider(TestConfig {
    endpoint: "memory://after-reload".to_string(),
});
```

A pre-run override is visible to reachable eager providers and the first service generation. A runtime override mutates the provider binding: dependent services and `Watch` triggers reload, and the next generation resolves the new daemon-local provider value.

> [!WARNING]
> **Deadlock Risk**: Avoid reaching into daemon internals in tests if you plan to `await` anything afterwards. Holding internal `DashMap` guards across `.await` points can deadlock when a service tries to access those same resources. Always prefer `SimulationHandle` readers such as `get_shelf()` and `get_status()`.

## 3. Troubleshooting

| Issue | Potential Solution |
| :--- | :--- |
| **Provided trait error** | Ensure the type has a `#[provider]` annotation. |
| **Trigger not firing** | Check if the module is included in `main.rs`. See [Registry Discovery](faq.md#1-registry--discovery). |
| **Sync warning in logs** | Use `async fn` or add `#[allow(sync_handler)]` on your service/trigger. |
| **Simulation hang in CI** | Likely a deadlock caused by holding `resources()` locks across an `.await`. Use `get_shelf()`. |
| **Registry interference** | All tests share the same `linkme` registry. Use `Registry::builder().with_tag("...")` to isolate. |

### Registry Isolation in Tests

Because `linkme` registers all services in the workspace, you may encounter interference between tests if multiple daemons try to run the same auto-registered service.

**Best Practice**:
1. Tag your services: `#[service(tags = ["core"])]`.
2. In your test, create a filtered registry:
   ```rust
   let reg = Registry::builder().with_tag("core").build();
   ServiceDaemon::builder().with_registry(reg).build();
   ```
For more details, see [Registry Isolation in FAQ](faq.md#4-testing--simulation).

[Back to README](../../README.md)
