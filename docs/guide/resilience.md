# Resilience & Lifecycle Management

This guide explains how `ServiceDaemon` ensures application stability through automatic restarts, priority-based orchestration, and graceful shutdown.

## 1. Automatic Restarts: Exponential Backoff & Jitter

Services that fail (return `Err`) are automatically restarted with exponential backoff and **randomized jitter** to prevent thundering herd issues. Clean `Ok(())` exits also start a new generation and restart immediately, without advancing the backoff counter.

```rust
use service_daemon::{ServiceDaemon, RestartPolicy};
use std::time::Duration;

let policy = RestartPolicy::builder()
    .initial_delay(Duration::from_secs(2))
    .max_delay(Duration::from_secs(300))
    .multiplier(1.5)
    .jitter_factor(0.1) // 10% randomization
    .build();

let mut daemon = ServiceDaemon::builder()
    .with_restart_policy(policy)
    .build();
daemon.run().await;
daemon.wait().await?;
```

### 1.1. Backoff, Jitter & Restart Storm Protection
The framework uses a unified `BackoffController` to manage retry delays, consecutive failure counts, and interruption-aware waiting. This ensures that both standard services and trigger handlers follow the same resilience policy.

Service supervisors also apply an internal restart-storm guard for pathological service failure loops. When repeated backoff-eligible service failures happen inside a short window, the supervisor may extend the effective restart delay. This guard is internal and conservative: services still retry indefinitely unless they return `ServiceError::Fatal`, clean `Ok(())` exits still restart immediately, and reload/shutdown signals still interrupt restart waits.

> [!NOTE]
> **Internal Architecture**: For the `BackoffController` state machine, restart-storm guard, and self-healing reset logic, see [Architecture: Lifecycle Management - Backoff Internals](../architecture/lifecycle-management.md#15-backoffcontroller-internals).

### 1.2. Retry Design: Services vs. Triggers

The framework uses a **two-tier retry design** that reflects the fundamentally different lifetimes of services and trigger handlers:

| Layer | Retry Behavior | How to Stop |
| :--- | :--- | :--- |
| **Service** | Restarts forever; failures use backoff plus an internal storm guard, clean exits restart immediately without backoff | Return `ServiceError::Fatal` from the service function |
| **Lazy Provider** | Resolves on demand during service runtime | Return `ProviderError::Fatal` from the provider, which triggers daemon shutdown |
| **Trigger dispatch** | A handler failure is retried inside the current dispatch; retry exhaustion becomes a recoverable trigger-service generation failure | Set `trigger_max_retries` on the `RestartPolicy` to bound each dispatch |

**Why the difference?** Services are long-running background tasks - they *are* the application. If a service crashes, the daemon must bring it back. The only valid reason for a service to stop permanently is an unrecoverable error (e.g., a missing license key, a corrupt database), which the service itself signals via `ServiceError::Fatal`.

Lazy providers are different: they may initialize after startup, inside a running service. In that case, a `ProviderError::Fatal` is promoted to a daemon-wide shutdown request by the service runner, so the process can stop cleanly instead of continuing in a partially initialized state.

Trigger handlers process individual events. A single handler `Err` is treated as a message-handling failure and stays inside the trigger retry pipeline. If the configured retry limit is exhausted, the trigger service generation reports a recoverable failure to the normal supervisor, which then applies the same restart/backoff/status/diagnostics path as other recoverable service failures.

### 1.3. Trigger Retry Safety Valve: `trigger_max_retries`

For trigger handlers that should **not** retry forever, set an explicit retry limit:

```rust
let policy = RestartPolicy::builder()
    .initial_delay(Duration::from_secs(1))
    .trigger_max_retries(5) // Give up after 5 consecutive failures
    .build();

let mut daemon = ServiceDaemon::builder()
    .with_restart_policy(policy)
    .build();
```

When `trigger_max_retries` is reached, the current dispatch is exhausted and the trigger service generation reports a recoverable failure. The supervisor then records the exit, applies restart/backoff policy, and starts the next generation when policy allows. The default is `None` (unlimited retries).

> [!WARNING]
> Do **not** use `trigger_max_retries` as a service restart-storm control. It only bounds retries for one trigger dispatch. Service-generation restarts remain governed by supervisor restart/backoff policy.

### 1.4. Fatal Errors

Sometimes a service encounters an error that it cannot recover from via a restart (e.g., a missing required environment variable or an invalid license). In such cases, the service should return `ServiceError::Fatal`.

When a service returns a `Fatal` error, the `ServiceDaemon` will **permanently stop** that service and transition its status to `Terminated`, bypassing the restart policy entirely.

```rust
use service_daemon::ServiceError;

#[service]
async fn license_checker() -> anyhow::Result<()> {
    if !check_license().await {
        return Err(ServiceError::Fatal("Invalid license key".into()).into());
    }
    // ...
    Ok(())
}
```

## 2. Initialization Resilience: Providers

When a provider's initialization fails, the daemon distinguishes transient errors from terminal provider-init boundary failures. User provider functions opt into this behavior by returning `Result<T, ProviderError>`; framework-generated providers can also produce `ProviderInitError` for required environment variables, parse failures, dependency-provider failures, panic translation, timeout, cancellation, and eager dependency-graph defense errors.

### 2.1. Provider Error Mapping: Retryable vs Fatal

When a provider fails to initialize, it can influence the daemon's behavior by returning specific error variants:

- **Retryable**: the daemon retries initialization with provider-init backoff until `RestartPolicy::provider_init_timeout` expires. If the timeout expires, the terminal boundary error is `ProviderInitError::Timeout`.
- **Fatal**: the daemon does not retry the provider. The terminal boundary error is `ProviderInitError::Fatal`, and the supervisor requests daemon shutdown for lazy failures or aborts startup for eager failures.

Provider retry/backoff is separate from service-generation restart/backoff. A provider-init terminal error bypasses the normal service restart loop; it is recorded as a provider-init lifecycle exit and does not synthesize a restart decision.

Cancellation also remains distinct: if daemon shutdown cancels provider initialization, the framework reports `ProviderInitError::Cancelled` rather than rewriting it as fatal.

### 2.2. Smart Listen Strategy (`Listen` Template)

The `Listen` template includes built-in intelligent error mapping for common I/O issues:

| OS Error | Strategy | Reason |
| :--- | :--- | :--- |
| `AddrInUse` | **Retryable** | Port is occupied, likely by an old instance still shutting down during rolling updates. |
| `Interrupted` | **Retryable** | System signal interrupted the bind operation. |
| `PermissionDenied` | **Fatal** | Attempted to bind to a low port (e.g., 80) without root privileges. |
| `AddrNotAvailable` | **Fatal** | Attempted to bind to an IP address that doesn't exist on the host. |

### 2.3. UnixListen Strategy (Unix Domain Socket Listener)

The `UnixListen` template diverges from `Listen` on one critical point: `AddrInUse` on a Unix socket **almost always means a stale socket file** from an unclean shutdown rather than a live process holding the port. Silently unlinking would clobber a legitimately running second daemon, so `UnixListen` first probes with `UnixStream::connect`:

- If a live process answers the probe -> **Fatal** ("held by another live process"). The framework refuses to bind.
- If the probe fails (connection refused, file is a regular file, etc.) -> the path is treated as stale, `unlink`ed, and bind proceeds.

| OS Error | Strategy | Reason |
| :--- | :--- | :--- |
| `AddrInUse` (after unlink) | **Retryable** | Race condition: another process recreated the path between our unlink and bind. Retry the detect-then-bind dance. |
| `Interrupted`, `TimedOut` | **Retryable** | System signal during bind. |
| `PermissionDenied` | **Fatal** | Parent directory not writable, or socket file owned by another user. |
| `NotFound` | **Fatal** | Parent directory does not exist (the framework does **not** auto-create it). |
| `InvalidInput` | **Fatal** | Path exceeds platform `sun_path` limit (~108 bytes Linux, ~104 bytes macOS). |

The probe-then-unlink path emits a `tracing::warn!` event with `provider` and `path` fields when a stale file is removed, so operators investigating "who deleted my socket file" have a framework-side breadcrumb.

### 2.4. UnixConnect Strategy (Unix Domain Socket Client)

The `UnixConnect` template performs **one connectivity probe at provider init time** and discards the result. The probe serves two purposes:

1. With `eager = true`, it blocks the system startup wave until the peer is reachable. Use this for adapter-style daemons that depend on a sidecar / supervisor that must be up before our own services start.
2. Fail-fast on misconfiguration: a typo in the path becomes `Fatal` at init time rather than at the first `connect()` somewhere in the hot path.

Peer servers will observe a single `accept()` followed by an instant close from the probe -- this is normal and any reasonable server already handles port-scanner / health-probe traffic the same way.

| OS Error | Strategy | Reason |
| :--- | :--- | :--- |
| `ConnectionRefused` | **Retryable** | Peer hasn't called `accept()` yet (peer is starting up). |
| `NotFound` | **Retryable** | Peer hasn't created the socket file yet (peer init in progress). |
| `ConnectionAborted` | **Retryable** | Peer accepted but immediately closed -- a startup race. |
| `Interrupted`, `TimedOut` | **Retryable** | System signal during connect. |
| `PermissionDenied` | **Fatal** | EACCES on the path -- a permissions issue is not a transient state. |
| `InvalidInput` | **Fatal** | Path too long. |

> [!IMPORTANT]
> `NotFound` is **Retryable** for `UnixConnect` (peer is starting) but **Fatal** for `UnixListen` (parent directory missing). The same `io::ErrorKind` carries different meaning depending on which side of the connection you are.

After init succeeds, `connect().await?` opens a fresh independent `tokio::net::UnixStream` on each call. `try_connect().await?` remains available as the explicitly named lower-level helper. The framework intentionally does not pool -- UDS connections are local and cheap to recreate.

### 2.5. NamedPipeListen Strategy (Windows Named Pipe Server)

The `NamedPipeListen` template is Windows-only and uses Tokio's
`tokio::net::windows::named_pipe::ServerOptions`. It validates local-only names
at runtime: the path must begin with `\\.\pipe\` and must have a non-empty
suffix. Remote pipe paths are rejected as fatal configuration errors.

The first server instance is created with `reject_remote_clients(true)` and
`first_pipe_instance(true)`. That first-instance flag is the ownership check: if
another server already owns the pipe name, Windows reports a permission-style
create failure and the framework treats it as **Fatal**. Later instances created
by the listener manager do not use `first_pipe_instance(true)`.

| OS Error | Strategy | Reason |
| :--- | :--- | :--- |
| Initial first-instance collision | **Fatal** | Another server already owns the pipe name; retrying would hide a deployment ownership conflict. |
| Invalid or remote pipe name | **Fatal** | The provider contract is local-only `\\.\pipe\...`. |
| `Interrupted`, `TimedOut` during create | **Retryable** | Transient system interruption while creating the server instance. |
| Other create/configuration/access errors | **Fatal** | The operator or configuration must change. |

`accept().await?` receives connected `NamedPipeServer`s from a lazily started
listener manager. The manager owns the pending instance, creates the next
instance after each connection, and retries runtime replacement-create failures
with short backoff. A successful `accept()` means the returned server end is
connected and ready for business logic; it does not require replacement creation
to have succeeded first. Provider-init classification still applies to
initialization, while runtime pending-create failures are manager recovery events
unless the manager stops.

### 2.6. NamedPipeConnect Strategy (Windows Named Pipe Client)

The `NamedPipeConnect` template stores only the local pipe name. Initialization
performs a one-shot `ClientOptions::new().open(...)` probe and drops it, matching
the Unix connector pattern: `eager = true` can block startup until a peer process
is reachable, while lazy initialization validates the peer on first resolution.

| OS Error | Strategy | Reason |
| :--- | :--- | :--- |
| `NotFound` | **Retryable** | The peer has not created the named pipe yet. |
| Raw OS `ERROR_PIPE_BUSY` (`231`) | **Retryable** | The pipe exists, but every server instance is currently occupied. |
| `Interrupted`, `TimedOut` | **Retryable** | Transient system interruption during open. |
| `PermissionDenied` | **Fatal** | Access or security configuration is wrong, including denied local access. |
| Invalid or remote pipe name | **Fatal** | The provider contract is local-only `\\.\pipe\...`. |
| Other configuration/access errors | **Fatal** | Retrying cannot fix malformed configuration or incompatible security settings. |

After init succeeds, `connect().await?` and `try_connect().await?` open fresh
independent `NamedPipeClient`s. A runtime `connect()` can still hit
`ERROR_PIPE_BUSY` if all server instances are occupied; retry that at the call
site when the workflow expects short-lived busy windows.

## 3. Advanced Resilience: Wave Timeouts

The `RestartPolicy` also controls how long the daemon waits for services during startup and shutdown waves.

```rust
let policy = RestartPolicy::builder()
    .wave_spawn_timeout(Duration::from_secs(10)) // Wait up to 10s for Healthy status
    .wave_stop_timeout(Duration::from_secs(45))  // Wait up to 45s for graceful stop
    .build();
```

- **Spawn Timeout**: The maximum time a startup wave waits for all services within it to report `Healthy`. If the timeout is reached, the daemon logs a warning and proceeds to the next wave to avoid blocking the entire system.
- **Stop Timeout**: The maximum time a shutdown wave waits for all services within it to exit gracefully before forcing an abort.

### 3.1. Queue concurrency and backpressure
For streaming triggers (e.g. `Queue`), [`ScalingPolicy`] controls handler concurrency separately from `RestartPolicy`. Scaling policy controls concurrency volume; restart policy controls retry/backoff time. Each trigger template declares its concurrency requirements via `TriggerHost::scaling_policy()`. Templates that do not need concurrent dispatch (e.g. `Cron`, `Watch`, `Notify`) return `None` and run serially. Most users can rely on the defaults or override them with `ScalingPolicy::builder()`.

## 4. Managing CPU-Intensive & Blocking Tasks

The asynchronous executor (Tokio) relies on cooperative multitasking. If a service performs a long-running CPU computation or a blocking I/O operation without yielding, it will **stall the entire daemon**.

### CPU-Intensive Tasks
Use `tokio::task::spawn_blocking` to offload heavy calculations:

```rust
#[service]
async fn compute_service() -> anyhow::Result<()> {
    while !service_daemon::is_shutdown() {
        let result = tokio::task::spawn_blocking(|| {
            perform_heavy_calculation()
        }).await?;
        
        service_daemon::sleep(Duration::from_secs(1)).await;
    }
    Ok(())
}
```

### The `#[allow(sync_handler)]` Escape Hatch
If your function is synchronous but guaranteed to be fast and non-blocking (e.g., in-memory math), use `#[allow(sync_handler)]` to suppress runtime warnings.

**How it works**: The macro system uses **AST-based attribute stripping**. It detects `#[allow(sync_handler)]`, sets a flag to bypass the async-wrapper requirement, and then **strips the attribute** before passing the code to the compiler. This prevents "unknown lint" errors while enabling synchronous execution.

```rust
#[service]
#[allow(sync_handler)] // Stripped by macro to avoid unknown lint errors
pub fn fast_calc() -> anyhow::Result<()> { Ok(()) }
```

> [!WARNING]
> **Never** use `#[allow(sync_handler)]` for network requests or disk I/O. This will cause severe performance degradation and may block shutdown.

## 5. Lifecycle Priorities

Services are assigned a `u8` priority (default 50) to determine their relative importance.
- **Startup**: Descending order (100 -> 0). Core systems start first.
- **Shutdown**: Ascending order (0 -> 100). Core systems stop last.

| Level (u8) | Constant | Purpose |
| :--- | :--- | :--- |
| **100** | `SYSTEM` | Core systems (Logging, Metrics). |
| **80** | `STORAGE` | Data providers, database pools. |
| **50** | `DEFAULT` | Core business logic and triggers. |
| **0** | `EXTERNAL` | API Gateways, HTTP servers. |

```rust
#[service(priority = ServicePriority::SYSTEM)]
pub async fn log_flush() { ... }
```

## 6. Graceful Shutdown

The daemon uses `CancellationToken` to signal services to stop. 
1. **Notification**: All services are notified via `is_shutdown()`.
2. **Error Suppression**: If a service exits with an error *after* the shutdown signal has been sent, the daemon treats it as a successful exit. This prevents irrelevant error logs (e.g., "channel closed" or "network unreachable") that naturally occur during the teardown of dependencies.
3. **Grace Period**: The daemon waits for a grace period (default: 30s) per wave.
4. **Forced Abort**: Services that don't exit within the period are aborted.

[Back to README](../../README.md)
