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

### Unit Testing with MockContext (God Mode)

Testing background services is difficult. How do you test a database failure at 2 AM? The `simulation` feature gives you total control ("God Mode") over the environment.

Enable the feature in your `Cargo.toml`:
```toml
[dev-dependencies]
service-daemon = { path = "...", features = ["simulation"] }
```

Use `MockContext` to create an isolated sandbox and `SimulationHandle` (The God's Hand) to reach into the running engine:

```rust
#[tokio::test]
async fn test_two_phase_simulation() {
    // 1. Setup Sandbox: Pre-fill the Shelf
    let (builder, handle) = MockContext::builder()
        .with_shelf::<String>("my_service", "config_key", "initial_val".into())
        .build();

    let daemon = builder.with_service(my_service).build();
    let cancel = daemon.cancel_token();
    
    // Start daemon in background
    let daemon_task = tokio::spawn(async move { daemon.run().await; });

    // 2. Mid-flight Intervention
    // Reach in and change the world!
    handle.set_shelf::<String>("my_service", "dynamic_key", "new_val".into());

    // 3. Verify side-effects
    let result = handle.get_shelf("my_service", "processed_result");
    assert!(result.is_some());

    cancel.cancel();
}
```

#### MockContext & SimulationHandle Capabilities

| Component | Method | Description |
| :--- | :--- | :--- |
| **Builder** | `with_shelf` | Pre-fills a Shelf entry for a specific service. |
| **Builder** | `with_status` | Pre-sets a lifecycle status in the isolated status plane. |
| **Handle** | `get_shelf` | Reads a cloned value from the shelf (Safe/Lock-free). |
| **Handle** | `get_status` | Reads a cloned status value (Safe/Lock-free). |
| **Handle** | `set_shelf` | Dynamically injects/overwrites a value in the shelf. |
| **Handle** | `set_status` | Dynamically flips a service's status (triggers reloads). |
| **Handle** | `trigger_reload` | Manually fires a reload signal for a service. |

> [!WARNING]
> **Deadlock Risk**: Avoid using `handle.resources()` directly in tests if you plan to `await` anything afterwards. Holding a reference to internal `DashMap` guards across `.await` points will cause an immediate deadlock when a service tries to access those same resources. Always prefer `get_shelf()` and `get_status()`.

## 3. Troubleshooting

| Issue | Potential Solution |
| :--- | :--- |
| **Provided trait error** | Ensure the type has a `#[provider]` annotation. |
| **Trigger not firing** | Check if the module is included in `main.rs`. See [Registry Discovery](pitfalls-faq.md#1-registry--discovery). |
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
For more details, see [Registry Isolation in FAQ](pitfalls-faq.md#4-testing--simulation).

[Back to README](../../README.md)
