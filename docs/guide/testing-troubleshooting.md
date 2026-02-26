# Testing & Troubleshooting

## 1. Common Patterns

### Resource Pooling
Use `#[provider]` for shared resources like database pools.

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
Verify:
- Priority-based startup/shutdown order.
- Status transitions and shelving correctness.
- Signal propagation and trigger execution.

### Unit Testing with MockContext

For isolated unit tests that don't spin up the full daemon, enable the `simulation` feature and use `MockContext`:

```toml
[dev-dependencies]
service-daemon = { path = "...", features = ["simulation"] }
```

`MockContext` provides a scoped, task-local environment proxy that shadows Providers, Shelf, and Status without touching global state. Multiple `MockContext` instances can run **in parallel** without interference.

```rust
use service_daemon::{MockContext, ServiceStatus};

#[tokio::test]
async fn test_my_service_complex_logic() {
    let ctx = MockContext::builder()
        .with_service_name("my_service")
        // Multiple providers can be mocked by chaining calls
        .with_mock::<AppConfig>(AppConfig { port: 9090 })
        .with_mock::<DatabaseConfig>(DatabaseConfig { url: "test_db".into() })
        // Multiple shelf entries for the same service
        .with_shelf::<i32>("counter", 100)
        .with_shelf::<String>("id", "uuid-123".into())
        // Mocking the status of dependency services
        .with_status(ServiceStatus::Healthy) // current service
        .with_service_status("AuthService", ServiceStatus::Healthy) // dependency
        .with_log_drain()
        .build();

    ctx.run(|| async {
        // AppConfig resolve() will return the mock value
        let config = AppConfig::resolve().await;
        assert_eq!(config.port, 9090);

        // DatabaseConfig also returns its respective mock value
        let db_config = DatabaseConfig::resolve().await;
        assert_eq!(db_config.url, "test_db");

        // unshelve() reads from the isolated shelf
        let counter: Option<i32> = service_daemon::unshelve("counter").await;
        assert_eq!(counter, Some(100));

        // state() returns the injected status
        assert_eq!(service_daemon::state(), ServiceStatus::Healthy);
    }).await;
}
```

#### MockContext Capabilities

| Builder Method       | Description                                                |
|---------------------|------------------------------------------------------------|
| `with_service_name` | Sets the service identity for shelf and status isolation.  |
| `with_mock::<T>`    | Injects a shadow Provider snapshot for type `T`. (Chainable) |
| `with_shelf`        | Pre-fills a Shelf entry. (Chainable for multiple keys)      |
| `with_status`       | Sets the current service lifecycle status.                 |
| `with_service_status` | Sets the status for a specific service (e.g. dependency). |
| `with_log_drain`    | Drains the internal log queue to stderr during the test.   |

> **Note**: The `simulation` feature is stripped from production builds via `#[cfg(feature = "simulation")]`, guaranteeing zero runtime overhead.

## 3. Troubleshooting

For common architectural traps and conceptual questions, please refer to the [Concept Clarification & Pitfalls (FAQ)](pitfalls-faq.md).

### Quick Fixes

| Issue | Potential Solution |
| :--- | :--- |
| **Provided trait error** | Ensure the type has a `#[provider]` annotation. |
| **Trigger not firing** | Check if the module is included in `main.rs`. See [Registry Discovery](pitfalls-faq.md#1-registry--discovery). |
| **Sync warning in logs** | Use `async fn` or `#[allow_sync]` on your service. |
| **Logs missing in tests** | Use `MockContext::builder().with_log_drain().build()`. |

### Registry Isolation in Tests

Because `linkme` registers all services in the workspace, you may encounter interference between tests. 

**Best Practice**:
1. Tag your services: `#[service(tags = ["core"])]`.
2. In your test, create a filtered registry:
   ```rust
   let reg = Registry::builder().with_tag("core").build();
   ServiceDaemon::builder().with_registry(reg)...
   ```
For a deep dive into why this happens, see [Registry Isolation in FAQ](pitfalls-faq.md#4-testing--simulation).

[Back to README](../../README.md)
