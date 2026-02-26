# 5-Minute Get Started Guide

Welcome! `service-daemon-rs` is designed to be invisible. If you find yourself fighting the framework, you're likely missing a core concept. This guide is your "First Peek" to ensure you start on the right foot.

---

## 1. The Core Loop: Declare & Forget

Framework implementation is a simple **Three-Step Dance**:

### Step 1: Define a Provider (The "What")
Don't worry about singletons or locks. Just define your type.

```rust
#[provider(default = 8080)]
pub struct AppPort(pub i32);
```

> [!TIP]
> Need something complex like MQTT? **Don't modify the macro.** Use an `async fn`:
> `#[provider] async fn mqtt() -> MqttBus { ... }`

### Step 2: Create a Service (The "How")
Inject your dependencies as `Arc<T>`.

```rust
#[service]
pub async fn my_web_server(port: Arc<AppPort>) -> anyhow::Result<()> {
    println!("Listening on port: {}", port.0);
    service_daemon::wait_shutdown().await;
    Ok(())
}
```

### Step 3: Run the Daemon (The "Run")
```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    ServiceDaemon::builder().build().run().await
}
```

---

## 2. The "Must-Know" Rules (Before You Code)

### Rule #1: The Module Visibility Rule
If your code isn't in a module reachable from `main.rs`, the framework **cannot** find it. 
*   **Wrong**: Just creating `my_svc.rs`.
*   **Right**: Adding `mod my_svc;` to your `main.rs`.

### Rule #2: One Paradigm Per Service
*   **Simple**: Use `service_daemon::wait_shutdown().await`.
*   **Reactive**: Use `while let Some(s) = state().match(...)`.
*   **Danger**: Don't mix them! Pick one and stick to it.

### Rule #3: Testing is Built-in
You don't need a database to test your service. Use `MockContext` to swap out the real `AppPort` with a test value.

---

## 3. Where to go next?

*   [**Deep Best Practices**](provider-best-practices.md): Learn the difference between "Magic Primitives" and "Business Components".
*   [**Common Pitfalls**](pitfalls-faq.md): Read this if your service isn't starting or triggers aren't firing.
*   [**Full Examples**](../../examples/): See real-world usage in the `examples/` directory.

---

> [!NOTE]
> **Pro Insight**: `service-daemon-rs` uses `linkme` for zero-config registration. It's powerful, but it requires that your code is part of the final binary. Modularity is key!
