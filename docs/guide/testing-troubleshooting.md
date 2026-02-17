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

### `the trait Provided is not implemented for T`
**Cause**: Missing `#[provider]` annotation on type `T` or its initializer fn.
**Fix**: Add `#[provider]`.

### Trigger Not Firing
**Cause**: Usually the module containing the trigger is not included in `main.rs` via `mod`.
**Fix**: Ensure `linkme` can find the trigger by including the module in the compilation tree.

### Sync Warning in Logs
**Cause**: Using `#[service]` on a `fn` instead of `async fn`.
**Fix**: Convert to `async fn` or use `#[allow_sync]` if truly non-blocking.

### Logs Not Visible in Tests
**Cause**: In unit tests, the `LogService` is not running, so internal log events are silently discarded.
**Fix**: Use `MockContext::builder().with_log_drain().build()` to drain logged events to stderr. Run with `cargo test -- --nocapture` to see them.

[Back to README](../../README.md)
