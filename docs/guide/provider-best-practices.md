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

"Magic" specifically refers to hardcoded templates inside the `#[provider]` macro. These templates generate specialized boilerplate that would be tedious to write manually.

| Template | Alias | Logic |
| :--- | :--- | :--- |
| `Notify` | `Event` | A `tokio::sync::Notify` wrapper for one-to-one or one-to-all signaling. |
| `Queue(T)` | `BQueue`, `BroadcastQueue` | A `tokio::sync::broadcast` channel for fan-out event distribution. |
| `Listen(Addr)` | - | A `std::net::TcpListener` wrapper with kernel-level FD cloning. Combined with `eager = true`, binds during the system startup wave; otherwise lazy on first injection. |
| `UnixListen(Path)` | - | **Unix-only.** A `std::os::unix::net::UnixListener` wrapper. Mirrors `Listen` but adds detect-and-unlink for stale socket files (refuses fatally if a live process holds the path). FD cloning via `try_get().await?`. |
| `UnixConnect(Path)` | - | **Unix-only.** Holds an `Arc<PathBuf>`; `try_connect().await?` opens a fresh `tokio::net::UnixStream` on each call. Performs a one-shot reachability probe at init time, so `eager = true` blocks the startup wave until the peer is ready. |

### The `Listen` Template

The `Listen` provider gives you a `std::net::TcpListener` wrapped so that multiple services can share the same port across reloads. Two relevant properties:
1. **OS-level sharing**: `get()` clones the underlying file descriptor via the kernel's `dup` syscall, so multiple services or reload generations can hold a `tokio::net::TcpListener` for the same physical port without conflicts.
2. **Environment fallback**: `#[provider(Listen("0.0.0.0:80"), env = "PORT")]` will pick up `PORT` if set, falling back to the literal otherwise.

Like every provider, `Listen` is **lazy by default** -- the bind happens the first time a service requests it. To bind the port during the system startup wave (the case you actually want for health probes and supervisor-style liveness checks), declare it with `eager = true` (see below).

### The `UnixListen` and `UnixConnect` Templates (Unix Domain Sockets)

These two templates form the UDS counterpart of `Listen`, designed to be used as a pair: one service runs the server-side accept loop, another service (or external process) initiates connections.

```rust
// Server side
#[derive(Clone)]
#[provider(UnixListen("/run/myapp/sock"), env = "MYAPP_SOCK")]
pub struct ApiSocket;

#[service]
pub async fn api_server(listener: Arc<ApiSocket>) -> anyhow::Result<()> {
    let l = listener.try_get().await?;
    loop {
        let (sock, _) = l.accept().await?;
        // handle sock...
    }
}

// Client side, in the same or a different daemon process
#[derive(Clone)]
#[provider(UnixConnect("/run/peer/sock"), env = "PEER_SOCK", eager = true)]
pub struct PeerClient;

#[service]
pub async fn peer_caller(client: Arc<PeerClient>) -> anyhow::Result<()> {
    let mut conn = client.try_connect().await?;
    // write/read on conn...
    Ok(())
}
```

Three behaviors that distinguish them from the TCP `Listen` template:

1. **`UnixListen` recovers from stale socket files**: an unclean shutdown leaves the socket file on disk, which on the next start would normally trigger `AddrInUse`. `UnixListen` first probes the path with `UnixStream::connect`. If a live process answers, the framework refuses fatally ("held by another live process"). Otherwise the path is treated as stale, `unlink`ed, and bind proceeds. This is the same pattern Docker uses for `/var/run/docker.sock`. See [Resilience Guide § 2.3](resilience.md#23-unixlisten-strategy-unix-domain-socket-listener) for the full error taxonomy.

2. **`UnixConnect` validates reachability at init**: the template performs a single `connect` probe and immediately drops the result. With `eager = true` this lets you block the startup wave until a peer sidecar / supervisor is up. Peer servers will see one extra `accept()` followed by an instant close per provider initialization -- treat it the same as port-scanner / health-probe traffic.

3. **Cross-platform builds**: both templates are gated by `#[cfg(unix)]`. On non-Unix targets the macro emits a `compile_error!` at the declaration site rather than silently producing a broken type. To write cross-platform code, wrap the declaration in `#[cfg(unix)] mod uds {...}` so the entire module is excluded on Windows.

> [!NOTE]
> **API form: both are `async`**. `try_get().await?` and `try_connect().await?` are uniformly async. `try_get`'s body is internally synchronous (`try_clone` + `set_nonblocking` + `from_std`) but the function is `async` so the call site stays consistent with the truly async `try_connect`. There is no runtime cost: the compiler inlines async fns with no await points.

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
| TCP Port Binding (lazy, on first inject) | `#[provider(Listen("0.0.0.0:80"))] struct HttpListener;` |
| TCP Port Binding (early-bound for probes) | `#[provider(Listen("0.0.0.0:80"), eager = true)] struct HealthListener;` |
| Unix Socket Listening (lazy) | `#[provider(UnixListen("/run/myapp/sock"))] struct ApiSocket;` |
| Unix Socket Listening (early-bound) | `#[provider(UnixListen("/run/myapp/sock"), eager = true)] struct ApiSocket;` |
| Unix Socket Connecting (lazy) | `#[provider(UnixConnect("/run/peer/sock"))] struct PeerClient;` |
| Unix Socket Connecting (block startup until peer ready) | `#[provider(UnixConnect("/run/peer/sock"), eager = true)] struct PeerClient;` |
| Early Background Task | `#[provider(eager = true)] async fn setup() -> () { ... }` |
