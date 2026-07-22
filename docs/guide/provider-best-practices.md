# Provider Strategy Guide

This guide helps you choose how to provide dependencies in your `service-daemon-rs` application. Matching the provider form to the resource keeps framework extension points focused.

---

## 1. Choosing Your Strategy

There are three ways to define a Provider. Choose based on your use case:

| Strategy | When to Use | Example |
| :--- | :--- | :--- |
| **Simple Value** | Static configuration, primitive types, or simple wrappers. | `Port(i32)`, `Config(String)` |
| **Async Function** | External systems, database connections, MQTT, heavy initialization. | `MqttBus`, `DatabasePool` |
| **Built-in template** | Low-level architecture primitives for synchronization, signaling, or networking. | `Notify`, `Listen`, `Queue` |

---

## 2. Prefer `#[provider] async fn` for custom resources

For custom providers such as MQTT, Redis, or HTTP clients, use an `async fn` provider instead of changing the framework's built-in templates.

Use the `#[provider]` attribute on an `async fn`:

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

### Why this works well
1. **No framework changes**: Application-specific resources stay in application code.
2. **Full initialization control**: Certificates, retries, and settings stay in the provider body.
3. **Scoped sharing**: Normal daemons share the root provider slot. Simulation overrides can replace that provider for one daemon without changing the provider definition.
4. **DI usage**: Inject `Arc<MqttBus>` into any `#[service]` just like a regular provider.

---

## 3. When to use a built-in template

Built-in templates are hardcoded forms inside the `#[provider]` macro. They generate repeated wrapper code for primitives that many applications need.

| Template | Alias | Logic |
| :--- | :--- | :--- |
| `Notify` | `Event` | A `tokio::sync::Notify` wrapper for one-to-one or one-to-all signaling. |
| `Queue(T)` | `BQueue`, `BroadcastQueue` | A `tokio::sync::broadcast` channel for fan-out event distribution. |
| `Listen(Addr)` | - | A `std::net::TcpListener` wrapper with kernel-level FD cloning. Combined with `eager = true`, binds during the system startup wave; otherwise lazy on first injection. |
| `UnixListen(Path)` | - | **Unix-only.** A `std::os::unix::net::UnixListener` wrapper. Mirrors `Listen` but adds detect-and-unlink for stale socket files (refuses fatally if a live process holds the path). Use `accept().await?` for the common accept loop or `get()?` for manual FD cloning. |
| `UnixConnect(Path)` | - | **Unix-only.** Holds an `Arc<PathBuf>`; `connect().await?` opens a fresh `tokio::net::UnixStream` on each call. Performs a one-shot reachability probe at init time, so `eager = true` blocks the startup wave until the peer is ready. |
| `NamedPipeListen(Name)` | - | **Windows-only.** Holds a local named pipe listener wrapper. `accept().await?` yields an already connected `NamedPipeServer`; an internal manager replenishes the next pending instance and retries replacement create failures. |
| `NamedPipeConnect(Name)` | - | **Windows-only.** Holds an `Arc<String>` pipe name. Init performs one `ClientOptions::open` probe; each `connect().await?` opens a fresh `NamedPipeClient`. |

### The `Listen` Template

The `Listen` provider gives you a `std::net::TcpListener` wrapped so that multiple services can share the same port across reloads. Two relevant properties:
1. **OS-level sharing**: `get()` clones the underlying file descriptor via the kernel's `dup` syscall, so multiple services or reload generations can hold a `tokio::net::TcpListener` for the same physical port without conflicts.
2. **Environment fallback**: `#[provider(Listen("127.0.0.1:8080"), env = "PORT")]` will pick up `PORT` if set, falling back to the literal otherwise.

Like every provider, `Listen` is **lazy by default** -- the bind happens the first time a service requests it. To bind the port during the system startup wave (the case you actually want for health probes and supervisor-style liveness checks), declare it with `eager = true` (see below).

Use loopback addresses for local-only services. Binding to `0.0.0.0` exposes
the listener on external interfaces and belongs in deployment-specific
configuration with firewall, authentication, rate-limit, TLS or reverse-proxy
controls already designed.

### The `UnixListen` and `UnixConnect` Templates (Unix Domain Sockets)

These two templates form the UDS counterpart of `Listen`, designed to be used as a pair: one service runs the server-side accept loop, another service (or external process) initiates connections.

```rust
// Server side
#[derive(Clone)]
#[provider(UnixListen("/run/myapp/sock"), env = "MYAPP_SOCK")]
pub struct ApiSocket;

#[service]
pub async fn api_server(listener: Arc<ApiSocket>) -> anyhow::Result<()> {
    loop {
        let (sock, _) = listener.accept().await?;
        // handle sock...
    }
}

// Client side, in the same or a different daemon process
#[derive(Clone)]
#[provider(UnixConnect("/run/peer/sock"), env = "PEER_SOCK", eager = true)]
pub struct PeerClient;

#[service]
pub async fn peer_caller(client: Arc<PeerClient>) -> anyhow::Result<()> {
    let mut conn = client.connect().await?;
    // write/read on conn...
    Ok(())
}
```

Three behaviors that distinguish them from the TCP `Listen` template:

1. **`UnixListen` recovers from stale socket files**: an unclean shutdown leaves the socket file on disk, which on the next start would normally trigger `AddrInUse`. `UnixListen` first probes the path with `UnixStream::connect`. If a live process answers, the framework refuses fatally ("held by another live process"). If the probe fails, the path is unlinked only after the framework confirms that the path itself is a Unix socket; ordinary files and other filesystem nodes are refused and preserved. See [Resilience Guide § 2.3](resilience.md#23-unixlisten-strategy-unix-domain-socket-listener) for the full error taxonomy.

2. **`UnixConnect` validates reachability at init**: the template performs a single `connect` probe and immediately drops the result. With `eager = true` this lets you block the startup wave until a peer sidecar / supervisor is up. Peer servers will see one extra `accept()` followed by an instant close per provider initialization -- treat it the same as port-scanner / health-probe traffic.

3. **Cross-platform builds**: both templates are gated by `#[cfg(unix)]`. On non-Unix targets the macro emits a `compile_error!` at the declaration site rather than silently producing a broken type. To write cross-platform code, wrap the declaration in `#[cfg(unix)] mod uds {...}` so the entire module is excluded on Windows.

> [!NOTE]
> **API form: listener handle cloning is synchronous; socket operations are `async`**. Use `accept().await?` and `connect().await?` for the common server/client paths. `get()?` and `try_connect().await?` remain available when you need the lower-level listener clone or explicitly named connection helper.

### The `NamedPipeListen` and `NamedPipeConnect` Templates (Windows Named Pipes)

Windows named pipes are the Windows-side local IPC templates. They are explicit
Windows templates, not aliases for `UnixListen` or `UnixConnect`:

```rust
// Server side
#[derive(Clone)]
#[provider(NamedPipeListen(r"\\.\pipe\myapp-api"))]
pub struct ApiPipe;

#[service]
pub async fn pipe_server(listener: Arc<ApiPipe>) -> anyhow::Result<()> {
    loop {
        let pipe = listener.accept().await?;
        // read/write on pipe...
    }
}

// Client side
#[derive(Clone)]
#[provider(NamedPipeConnect(r"\\.\pipe\peer-api"), eager = true)]
pub struct PeerPipe;

#[service]
pub async fn peer_caller(client: Arc<PeerPipe>) -> anyhow::Result<()> {
    let mut conn = client.connect().await?;
    // write/read on conn...
    Ok(())
}
```

Use these templates when the target deployment is Windows and the dependency is
a local named pipe endpoint. Use the Unix templates for Unix domain sockets, and
use `Listen` for TCP sockets. The framework intentionally does not make one
template name change behavior across operating systems.

`NamedPipeListen` validates local-only pipe names, creates the first instance
with `reject_remote_clients(true)` and `first_pipe_instance(true)`, then lazily
starts a listener manager on the first `accept().await?`. Successful accepts
return already connected `NamedPipeServer`s. The manager owns pending instances,
replenishes the next one after each connection, and retries replacement-create
failures internally instead of pushing those transient failures onto business
handlers. ACL and security-descriptor customization is not part of the template
API yet.

`NamedPipeConnect` validates the same local-only pipe name form. Initialization
performs a one-shot reachability probe and drops it; `connect().await?` and
`try_connect().await?` open fresh independent clients.

Both named pipe templates are gated by `#[cfg(windows)]`. On non-Windows
targets, the macro emits a declaration-site `compile_error!`. To keep a
cross-platform crate compiling, place the declaration in a `#[cfg(windows)]`
module and provide a Unix or TCP alternative in a separate cfg branch.

**Avoid creating new built-in templates unless:**
* You are implementing a **generic synchronization primitive** used across many different projects.
* The provider requires **special code generation** (like automatically creating `push()`, `subscribe()`, or `get()` instance methods via macro).

> [!IMPORTANT]
> Business-specific components (MQTT, Database, API Clients) should be implemented as regular `async fn` providers.

---

## 4. Initialization Control: The `eager` Flag

By default, providers are **lazy**; they are only initialized when a service first requests them. If you need a provider to start immediately during the daemon's startup phase, use `eager = true`:

> [!NOTE]
> **Reachable Eager**: A provider marked as `eager` is only initialized if it is **reachable** from your registered services. If no service depends on it (directly or indirectly), it will stay uninitialized to save resources.

```rust
#[provider(Listen("127.0.0.1:8080"), eager = true)]
pub struct WebListener;
```

---

## 5. Choosing `ProviderError::Fatal` vs `ProviderError::Retryable`

Use `ProviderError` only from provider functions that intentionally opt into framework-owned initialization semantics by returning `Result<T, ProviderError>`.

Return `ProviderError::Fatal` when retrying cannot make progress without an operator or configuration change:

- required credentials or configuration are malformed;
- a local filesystem or permission problem is deterministic;
- a peer contract is incompatible with the current binary.

Return `ProviderError::Retryable` when the same initialization may succeed soon without changing code or configuration:

- a dependent process is still starting;
- a socket or port is temporarily unavailable during rolling restart;
- a short network or service discovery outage is expected to clear.

Retryable provider errors are bounded by `RestartPolicy::provider_init_timeout`. Once that timeout expires, the framework reports a `ProviderInitError::Timeout`; it does not convert cancellation or timeout into a generic fatal error. Normal service code should still receive providers through DI rather than catching these initialization errors itself.

---

## 6. Helper APIs Are Usually Not the Main Path

Most applications should not call provider helper methods directly. Declare providers, inject `Arc<T>` / `Arc<RwLock<T>>` / `Arc<Mutex<T>>` into services or triggers, and let the daemon own initialization, retry, cancellation, and reload behavior.

When helper methods are called from a service, trigger, watcher, or daemon startup path, they use that daemon's effective provider scope. When helper methods are called outside framework context, they use the root fallback slot. Treat that fallback as a convenience for tests, setup, and diagnostics rather than as a production override mechanism.

If you are writing tests, diagnostics, or macro-level integrations and need the exact helper return shapes, see [Macro Expansion](../architecture/macro-expansion.md#provider-helper-return-shapes).

---

## 7. Common Misconceptions

* **"I need a built-in template for my DB"**: No. Use an `async fn` provider that returns your connection pool.
* **"Built-in templates are faster"**: No. They use the same `StateManager` and capability traits (`Provided` / `ManagedProvided` / `WatchableProvided`) under the hood. They are shorthand for common primitives.
* **"Provider overrides should be global"**: No. Test-time overrides belong to a simulation daemon scope so they do not pollute root helper resolution or other daemon instances.
* **"Provided is hard to implement"**: You should **never** implement provider capability traits manually for normal usage. Let `#[provider]` do it for you.

---

## 8. Summary Table

| Goal | Best Approach |
| :--- | :--- |
| Inject a constant | `#[provider(80)] struct Port(i32);` |
| Inject a DB Connection | `#[provider] async fn db() -> Pool { ... }` |
| Signal between services | `#[provider(Notify)] struct Signal;` |
| Fan-out events | `#[provider(Queue(String))] struct Bus;` |
| TCP Port Binding (lazy, on first inject) | `#[provider(Listen("127.0.0.1:8080"))] struct HttpListener;` |
| TCP Port Binding (early-bound for probes) | `#[provider(Listen("127.0.0.1:8080"), eager = true)] struct HealthListener;` |
| Unix Socket Listening (lazy) | `#[provider(UnixListen("/run/myapp/sock"))] struct ApiSocket;` |
| Unix Socket Listening (early-bound) | `#[provider(UnixListen("/run/myapp/sock"), eager = true)] struct ApiSocket;` |
| Unix Socket Connecting (lazy) | `#[provider(UnixConnect("/run/peer/sock"))] struct PeerClient;` |
| Unix Socket Connecting (block startup until peer ready) | `#[provider(UnixConnect("/run/peer/sock"), eager = true)] struct PeerClient;` |
| Windows Named Pipe Listening (lazy) | `#[provider(NamedPipeListen(r"\\.\pipe\myapp-api"))] struct ApiPipe;` |
| Windows Named Pipe Connecting (block startup until peer ready) | `#[provider(NamedPipeConnect(r"\\.\pipe\peer-api"), eager = true)] struct PeerPipe;` |
| Early Background Task | `#[provider(eager = true)] async fn setup() -> () { ... }` |
