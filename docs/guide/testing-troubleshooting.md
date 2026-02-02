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

[Back to README](../../README.md)
