# Provider Best Practices & Strategy Guide

This guide helps you choose the right way to provide dependencies in your `service-daemon-rs` application. Using the correct strategy avoids unnecessary framework complexity and keeps your code clean.

---

## 1. Choosing Your Strategy

There are three ways to define a Provider. Choose based on your use case:

| Strategy | When to Use | Example |
| :--- | :--- | :--- |
| **Simple Value** | Static configuration, primitive types, or simple wrappers. | `Port(i32)`, `Config(String)` |
| **Async Function** | **(Recommended)** External systems, database connections, MQTT, heavy initialization. | `MqttBus`, `DatabasePool` |
| **Magic Provider** | Low-level architecture primitives for synchronization, signaling, or networking. | `Notify`, `Listen`, `Queue` |

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

"Magic" refers to hardcoded templates inside the `#[provider]` macro. These templates generate specialized boilerplate that would be tedious to write manually.

| Template | Alias | Logic |
| :--- | :--- | :--- |
| `Notify` | `Event` | A `tokio::sync::Notify` wrapper for one-to-one or one-to-all signaling. |
| `Queue(T)` | `BQueue`, `BroadcastQueue` | A `tokio::sync::broadcast` channel for fan-out event distribution. |
| `Listen(Addr)` | - | A production-grade `std::net::TcpListener` wrapper with FD cloning capability. |

### The `Listen` Template Excellence

The `Listen` provider is specifically designed for high-performance servers. Unlike a manual `TcpListener` bind:
1. **OS-Level Sharing**: It uses the kernel's `dup` syscall via the `get()` method, allowing multiple services to share the same physical port without conflicts during reloads.
2. **Environment Integration**: Supports automatic fallback to environment variables: `#[provider(Listen("0.0.0.0:80"), env = "PORT")]`.

**Avoid creating new Magic Providers unless:**
* You are implementing a **generic synchronization primitive** used across many different projects.
* The provider requires **special code generation** (like automatically creating `push()`, `subscribe()`, or `get()` instance methods via macro).

> [!IMPORTANT]
> Business-specific components (MQTT, Database, API Clients) are **NOT** Magic Providers. They should be implemented as regular `async fn` providers.

---

## 4. Initialization Control: The `eager` Flag

By default, providers are **lazy**; they are only initialized when a service first requests them. If you need a provider to start immediately during the daemon's startup phase, use `eager = true`:

> [!NOTE]
> **Reachable Eager**: A provider marked as `eager` is only initialized if it is **reachable** from your registered services. If no service depends on it (directly or indirectly), it will stay uninitialized to save resources.

```rust
#[provider(Listen("0.0.0.0:80"), eager = true)]
pub struct WebListener;
```

---

## 5. Common Misconceptions

* **"I need a Magic Provider for my DB"**: No! Use an `async fn` provider that returns your connection pool.
* **"Magic Providers are faster"**: No! They use the same `StateManager` and capability traits (`Provided` / `ManagedProvided` / `WatchableProvided`) under the hood. They are just shorthand for common patterns.
* **"Provided is hard to implement"**: You should **never** implement provider capability traits manually for normal usage. Let `#[provider]` do it for you.

---

## 5. Summary Table

| Goal | Best Approach |
| :--- | :--- |
| Inject a constant | `#[provider(80)] struct Port(i32);` |
| Inject a DB Connection | `#[provider] async fn db() -> Pool { ... }` |
| Signal between services | `#[provider(Notify)] struct Signal;` |
| Fan-out events | `#[provider(Queue(String))] struct Bus;` |
| TCP Port Binding | `#[provider(Listen("0.0.0.0:80"))] struct HttpListener;` |
| Early Background Task | `#[provider(eager = true)] async fn setup() -> () { ... }` |
