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

In a simulation test, you don't run a real daemon. You use a `MockContext` to create a "God's eye view" of the system.

```rust
#[tokio::test]
async fn test_my_service_logic() {
    // 1. Build a sandbox
    let (builder, handle) = MockContext::builder()
        .with_shelf::<u32>("my_service", "counter", 10) // Pre-fill the shelf
        .build();

    // 2. Run your daemon in the sandbox
    let daemon = builder.build();
    
    // The handle gives you "God Powers" over the running services
    let runner = tokio::spawn(async move {
        daemon.run_for_duration(Duration::from_millis(500)).await.ok();
    });

    // ... perform your test assertions ...
}
```

## 3. The "God Hand" (`SimulationHandle`)

The `SimulationHandle` allows you to reach into the running sandbox and change things while the services are active.

```rust
// Mid-test: Change the status of a service to force a reload
handle.set_status(&service_id, ServiceStatus::NeedReload).await;

// Mid-test: Inject a new value into the shelf
handle.set_shelf::<String>("target_svc", "config_override", "MALICIOUS".into()).await;
```

## 4. Time Travel

Because the simulator can be used with `run_for_duration`, you can effectively "teleport" through time to see how your services behave over long periods (e.g., testing a log rotation service after 24 hours of uptime) without actually waiting for real hours to pass.

## 5. Summary of Powers

*   **Pre-populate the Shelf**: Test state recovery without waiting for a real crash.
*   **Dynamic Injection**: Overwrite dependencies at runtime.
*   **Status Flipping**: Force services into `NeedReload`, `Recovering`, or `ShuttingDown` to test their reaction logic.

---

[**← Previous Step: Waves of Orchestration**](orchestration-waves.md) | [**Next Step: Under the Hood →**](under-the-hood.md)
