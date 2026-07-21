# State Management

Effective state management is key to building reactive applications. This guide covers how to manage shared state and persistent service data.

## 0. Quick Start: The Heartbeat Pattern

Every service in `service-daemon-rs` revolves around **Providers** (Data) and **Services** (Logic).

```rust
use service_daemon::{provider, service, sleep, is_shutdown};
use std::sync::Arc;
use std::time::Duration;

// 1. Define a Provider (Type-Safe Shared State)
#[provider(5)]
pub struct HeartbeatInterval(pub u64);

// 2. Define a Service (The Business Logic)
#[service]
pub async fn heartbeat_service(interval: Arc<HeartbeatInterval>) -> anyhow::Result<()> {
    while !is_shutdown() {
        tracing::info!("Lub-dub...");
        sleep(Duration::from_secs(interval.0)).await;
    }
    Ok(())
}
```

> [!TIP]
> **Cancellation-aware sleep**: `service_daemon::sleep()` wakes when shutdown is requested, so services do not have to wait for the full duration before exiting.

---

> [!TIP]
> Unsure whether to use a Provider (State) or the Shelf? See the [State vs. Shelf comparison in the FAQ](faq.md#3-providers--state).

`service-daemon-rs` optimizes shared state synchronization based on how your services declare their dependencies.

## 1. Snapshots & Mutability Patterns

`StateManager` manages the transition between immutable snapshots and mutable tracked state. Services can request standard-looking `RwLock` or `Mutex` dependencies while the framework keeps the tracked state needed by `Watch` triggers.

### Snapshot and mutable state
Declare a dependency as `Arc<RwLock<T>>` or `Arc<Mutex<T>>` to gain write access.
- **Automatic promotion**: The provider moves to a `TrackedRwLock` on the first lock request.
- **Pointer replacement**: Use `guard.publish(Arc<T>)` to replace the state with a new shared value, which avoids cloning large values.

> [!NOTE]
> **Internal Mechanics**: For details on how `StateManager` manages transitions using `OnceCell` and `tokio::sync::watch`, see [Internal Architecture: State Management](../architecture/internal-overview.md#7-coremanaged_staters).

```rust
#[service]
pub async fn stats_updater(stats: Arc<RwLock<GlobalStats>>) -> anyhow::Result<()> {
    let mut guard = stats.write().await;
    
    // Path A: In-place mutation (requires T: Clone internally)
    guard.total_processed += 1;
    guard.commit(); 
    
    // Path B: Full replacement (Zero-Copy)
    let new_stats = Arc::new(compute_diff(&*guard));
    guard.publish(new_stats); 
    
    Ok(()) // Auto-commit on Drop fires only if DerefMut was invoked and commit() was not called manually
}
```

### Watch and reload boundaries

A provider value change belongs to the provider slot that published it. In the default case, daemon scopes inherit the root slot, so a managed value mutation can reload every selected service or trigger that still depends on that root slot. If a simulation daemon or internal fork shadows that provider with a daemon-local slot, local value changes reload only that daemon's dependents.

A provider binding change is different: it changes which slot a daemon uses for a provider type. The framework treats that as a generation-boundary reload, so the old generation exits and the next generation resolves the new provider slot.

### Advanced state inspection

Normal applications should use service/trigger injection and let the daemon manage provider initialization. Raw provider helper return values and `StateManager::snapshot()` preconditions are documented in [Lifecycle Management](../architecture/lifecycle-management.md#55-advanced-provider-helper-and-state-preconditions) for testing, diagnostics, and framework integrations.

## 2. Specialized Templates

`service-daemon-rs` provides several built-in templates for common infrastructure needs. Like all providers they are **lazy by default**; the templates ship the *capability* for early initialization but only run during the system startup wave when you also declare `eager = true` (covered in the next section).

### `Listen` (TCP listener with FD cloning)
The `Listen` template addresses cases where a TCP port must be bound *before* the rest of the application is ready -- typical for container health probes, supervisor liveness checks, or any external watcher that hits the socket as soon as the process is up.

If you call `TcpListener::bind()` inside a service, the port doesn't open until that service runs. If your DB migration or some other initialization takes 10 seconds, the watcher times out and assumes the process is dead. Pairing `Listen` with `eager = true` solves this by binding during the system startup wave, before any user service runs.

- **Bind timing**: lazy on first injection by default; bound during the system startup wave when declared `eager = true`.
- **FD cloning**: Each call to `listener.get()` returns a new `tokio::net::TcpListener` by cloning the underlying OS file descriptor (`dup`). This allows multiple services or reload generations to share the same port.
- **Resilience and auto-retry**: Built-in error mapping (see [Resilience Guide](resilience.md#22-smart-listen-strategy)). Transient errors like `AddrInUse` are retried with backoff; permission errors are fatal.

### `UnixListen` and `UnixConnect` (Unix domain sockets, Unix-only)

These two templates form the UDS counterpart of `Listen` and work as a pair: one service runs the accept loop via `UnixListen`, another runs the client side via `UnixConnect`. Both are gated by `#[cfg(unix)]`; on non-Unix targets the macro emits a `compile_error!` at the declaration site.

- **`UnixListen("/path/to/sock")`**: wraps `Arc<std::os::unix::net::UnixListener>`. `accept().await?` accepts a connection from a freshly cloned listener; use `get()?` when you need direct access to that cloned `tokio::net::UnixListener`. Critical difference: at init time, if the path already exists, the template probes with `UnixStream::connect`; a live process answering means the framework refuses fatally, while a failed probe only unlinks the path after confirming it is a Unix socket. Ordinary files and other filesystem nodes are refused and preserved.

- **`UnixConnect("/path/to/sock")`**: wraps `Arc<PathBuf>`. `connect().await?` opens a fresh `tokio::net::UnixStream` on each call (no pooling -- UDS connections are local and cheap); `try_connect().await?` is the equivalent lower-level helper. At init time the template performs one reachability probe and immediately drops the connection. Pair with `eager = true` to block the startup wave until the peer sidecar / supervisor is up.

```rust
#[derive(Clone)]
#[provider(UnixListen("/run/myapp/api.sock"), eager = true)]
pub struct ApiSocket;

#[derive(Clone)]
#[provider(UnixConnect("/run/peer/control.sock"), env = "PEER_SOCK")]
pub struct PeerClient;

#[service]
pub async fn web_server(api: Arc<ApiSocket>) -> anyhow::Result<()> {
    let (sock, _) = api.accept().await?;
    // handle sock ...
    Ok(())
}

#[service]
pub async fn supervisor_caller(peer: Arc<PeerClient>) -> anyhow::Result<()> {
    let mut conn = peer.connect().await?;
    // request/response ...
    Ok(())
}
```

Error classification details for both sides live in [Resilience Guide § 2.3-2.4](resilience.md#23-unixlisten-strategy-unix-domain-socket-listener). Note one subtlety: `io::ErrorKind::NotFound` is **Retryable** for `UnixConnect` (peer is starting) but **Fatal** for `UnixListen` (parent directory does not exist).

### `NamedPipeListen` and `NamedPipeConnect` (Windows named pipes, Windows-only)

These two templates are the Windows-side local IPC pair. They are explicit
Windows named pipe templates, not alternate behavior for `UnixListen` or
`UnixConnect`. Both are gated by `#[cfg(windows)]`; on non-Windows targets the
macro emits a `compile_error!` at the provider declaration site.

- **`NamedPipeListen(r"\\.\pipe\name")`**: wraps the server side. `accept().await?`
  returns a connected `tokio::net::windows::named_pipe::NamedPipeServer` and the
  generated wrapper keeps another pending server instance available for the next
  client.

- **`NamedPipeConnect(r"\\.\pipe\name")`**: wraps the client side. `connect().await?`
  opens a fresh `NamedPipeClient` on each call. At init time the template
  performs one reachability probe and immediately drops the client. Pair with
  `eager = true` when startup should wait for the peer pipe.

Use these templates for Windows local IPC only. Use `UnixListen` / `UnixConnect`
for Unix domain sockets and `Listen` for TCP sockets. Error classification
details live in [Resilience Guide § 2.5](resilience.md#25-namedpipelistennamedpipeconnect-strategy-windows-named-pipes).

### Eager Initialization: `eager = true`

Providers are lazy-initialized upon their first injection by default. For providers that must start regardless of injection (e.g., health-check listeners or global telemetry), the `eager = true` parameter forces initialization during the system startup wave.

```rust
#[derive(Clone)]
#[provider(Listen("127.0.0.1:8080"), eager = true)]
pub struct HealthListener;

#[provider(eager = true)]
pub async fn telemetry_init() -> StatsClient {
    // This will run immediately on startup regardless of injection
    init_tracing_pipeline().await
}
```

```rust
// In your providers definition:
#[derive(Clone)]
#[provider(Listen("127.0.0.1:8080"), env = "LISTEN_ADDR")]
pub struct ApiListener;

// In your service:
#[service(priority = ServicePriority::EXTERNAL)]
pub async fn web_server(listener: Arc<ApiListener>) -> anyhow::Result<()> {
    let listener = listener.get()?; // Clones the FD into a tokio listener
    axum::serve(listener, my_app)
        .with_graceful_shutdown(service_daemon::wait_shutdown())
        .await
        .map_err(Into::into)
}
```

For a complete Web API reference that combines an Axum HTTP service, explicit CORS policy, `utoipa-axum` OpenAPI routing, response envelopes, graceful shutdown, and a maintenance trigger, see `examples/web-api` (`cargo run -p example-web-api`).

### Signal & Queues
- **Notify**: Wraps `tokio::sync::Notify`. Ideal for manual triggers.
- **Queue / BQueue / BroadcastQueue**: Integrated event channels with configurable `capacity`.

## 3. Unified Status Plane

The Status Plane provides services with lifecycle awareness via the `ServiceStatus` enum.

| Status | Description |
|--------|-------------|
| `Initializing` | Fresh start |
| `Restoring` | Warm start with shelved data |
| `Recovering(err)`| Crash recovery with error context |
| `Healthy` | Normal operation |
| `NeedReload` | The current generation's reload token fired; save state and exit this generation |
| `ShuttingDown` | Shutdown in progress |
| `Terminated` | Service has exited and is ready for collection |

### Lifecycle Utilities
- `state()`: Get current status.
- `done()`: Signal initialization complete (prevents wave hangs).
- `is_shutdown()`: Check if service should stop.
- `sleep(duration)`: Interruptible async sleep.

### Standard Handoff (Destructive)
Use `unshelve()` when you only need to read the data once (e.g., at startup).
```rust
let history: Option<Vec<String>> = unshelve("history").await;
```

### Persistent Read (Non-Destructive)
Use `shelve_clone()` when you need to read the same data repeatedly without removing it from the shelf (e.g., inside a trigger's `handle_step`).
```rust
use service_daemon::shelve_clone;
let config: Option<AppConfig> = shelve_clone("app_config").await;
```

---


[Back to README](../../README.md)
