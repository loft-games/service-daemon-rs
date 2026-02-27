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

For isolated unit tests that don't spin up the full production registry, enable the `simulation` feature and use `MockContext`:

```toml
[dev-dependencies]
service-daemon = { path = "...", features = ["simulation"] }
```

`MockContext` acts as a **simulation sandbox factory**. It prepares isolated resources (Shelf, Status Plane) and yields a `SimulationHandle` (the "God Hand") to safely inspect or mutate state while the daemon is running in the background.

```rust
use service_daemon::{MockContext, ServiceStatus, Registry, ServiceId};
use std::time::Duration;

#[tokio::test]
async fn test_service_recovery_flow() {
    // 1. Setup Phase: Pre-fill isolated resources
    let (builder, handle) = MockContext::builder()
        .with_shelf::<String>("my_service", "last_job_id", "job-99".into())
        .with_status(ServiceId::new(0), ServiceStatus::Recovering("Previous crash".into()))
        .build();

    // 2. Build the Sandbox Daemon
    // Note: Simulation defaults to an isolated (empty) registry.
    let daemon = builder
        .with_service(my_service_description) // add the real service under test
        .build();

    let cancel = daemon.cancel_token();
    let daemon_handle = tokio::spawn(async move {
        let mut d = daemon;
        d.run().await;
        d.wait().await.unwrap();
    });

    // 3. Inspection & Mutation Phase (The God Hand)
    // Snapshot accessors are cross-await friendly (lock-free)
    let shelf_val: Option<String> = handle.get_shelf("my_service", "last_job_id");
    assert_eq!(shelf_val, Some("job-99".into()));

    // Mid-flight intervention: force a reload
    handle.set_status(ServiceId::new(0), ServiceStatus::NeedReload);
    
    // cleanup
    cancel.cancel();
    let _ = daemon_handle.await;
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
| **Sync warning in logs** | Use `async fn` or `#[allow_sync]` on your service. |
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
