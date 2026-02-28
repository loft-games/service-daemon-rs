# Provider Best Practices & Strategy Guide

This guide helps you choose the right way to provide dependencies in your `service-daemon-rs` application. Using the correct strategy avoids unnecessary framework complexity and keeps your code clean.

---

## 1. Choosing Your Strategy

There are three ways to define a Provider. Choose based on your use case:

| Strategy | When to Use | Example |
| :--- | :--- | :--- |
| **Simple Value** | Static configuration, primitive types, or simple wrappers. | `Port(i32)`, `Config(String)` |
| **Async Function** | **(Recommended)** External systems, database connections, MQTT, heavy initialization. | `MqttBus`, `DatabasePool` |
| **Magic Provider** | Low-level architecture primitives for synchronization/communication. | `Notify`, `BroadcastQueue` |

---

## 2. The Power of `#[provider] async fn`

For 95% of custom providers (like MQTT, Redis, or HTTP clients), you should **never** need to modify the framework's internal macro templates or "Magic Providers". 

Simply use the `#[provider]` attribute on an `async fn`:

```rust
use service_daemon::provider;

#[derive(Clone)]
pub struct MqttBus { /* ... */ }

#[provider]
pub async fn mqtt_provider() -> MqttBus {
    // 1. Complex initialization logic here
    let client = connect_to_mqtt().await;
    
    // 2. Background tasks (if needed)
    tokio::spawn(async move { /* lifecycle management */ });

    // 3. Return the type
    MqttBus { client }
}
```

### Why this is the Best Strategy:
1. **Zero Framework Bloat**: No need to touch `service-daemon` source code.
2. **Full Logic Control**: You have total control over certificates, retries, and settings.
3. **Implicit Singleton**: The framework ensures this `async fn` is only called **once**.
4. **Standard DI**: Inject `Arc<MqttBus>` into any `#[service]` just like a regular provider.

---

## 3. When is it a "Magic Provider"?

"Magic" refers to hardcoded templates inside the `#[provider]` macro (e.g., `#[provider(Notify)]`). 

**Avoid creating new Magic Providers unless:**
* You are implementing a **generic synchronization primitive** used across many different projects.
* The provider requires **special code generation** (like automatically creating `push()` or `subscribe()` methods via macro).

> [!IMPORTANT]
> Business-specific components (MQTT, Database, API Clients) are **NOT** Magic Providers. They should be implemented as regular `async fn` providers.

---

## 4. Common Misconceptions

* **"I need a Magic Provider for my DB"**: No! Use an `async fn` provider that returns your connection pool.
* **"Magic Providers are faster"**: No! They use the same `StateManager` and `Provided` trait under the hood. They are just shorthand for common patterns.
* **"Provided is hard to implement"**: You should **never** implement `Provided` manually. Let `#[provider]` do it for you.

---

## 5. Summary Table

| Goal | Best Approach |
| :--- | :--- |
| Inject a constant | `#[provider(80)] struct Port(i32);` |
| Inject a DB Connection | `#[provider] async fn db() -> Pool { ... }` |
| Signal between services | `#[provider(Notify)] struct Signal;` |
| Fan-out events | `#[provider(Queue(String))] struct Bus;` |
