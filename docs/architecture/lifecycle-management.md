# Lifecycle Management & Status Plane

The `ServiceDaemon` uses a structured orchestration system to manage service generations, crashes, and reloads.

## 1. Unified Status Plane

All services share a central **Status Plane** (`DashMap<ServiceId, ServiceStatus>`) managed by `DaemonResources`.

| Level | Transitions to | Triggered by |
|--------|----------------|--------------|
| `Initializing` | `Healthy` | `done()` or implicit handshake |
| `Restoring` | `Healthy` | Successful warm start or implicit handshake |
| `Recovering(err)`| `Healthy` | Custom recovery logic + `done()` or implicit handshake |
| `NeedReload` | `Terminated` | Service observes reload, performs cleanup, then calls `done()` |
| `ShuttingDown` | `Terminated` | Daemon shutdown signal + service cleanup |
| (Any) | `Terminated` | `ServiceError::Fatal` or daemon teardown |

`NeedReload` is primarily a **service-observed lifecycle state** exposed by `state()`. In the runtime, dependency watchers notify the supervisor through reload signals, which cancel the current generation's reload token. Once that token is cancelled, `state()` resolves to `NeedReload` immediately for the running service. The shared Status Plane remains the durable observation surface, but reload intent is delivered first through the token/signal control path rather than requiring a separate `Healthy -> NeedReload` map write.

> [!NOTE]
> **Signal handling**: The `ServiceSupervisor` uses one `tokio::select!` loop for service execution and signal bridging, so reload and shutdown signals do not need separate helper tasks.

### Control Plane Runtime and Declared Body Modes

Service supervisors, dependency watchers, startup wave orchestration, restart/backoff waits, shutdown coordination, and control diagnostics run on a daemon-owned control runtime. Service and trigger bodies execute through their statically declared scheduling mode:

- `Standard`: host Tokio runtime integration through the runtime that called `ServiceDaemon::run()`.
- `HighPriority`: daemon-owned low-contention high-priority runtime lane, created lazily with a worker count planned from final declared HighPriority entries.
- `Isolated`: a private OS thread and private Tokio runtime for each generation body.

The supervisor awaits body outcomes through the body-lane bridge, so reload, restart/backoff, fatal/provider-init handling, and shutdown coordination stay in the control plane even when the body runs elsewhere.

HighPriority capacity planning happens before runtime allocation and only reads the final daemon service list. Services and triggers are both `ServiceDescription` entries, so declared HighPriority triggers and services contribute equally to the planned worker count. Pressure diagnostics remain advisory and do not rebuild or resize the runtime after creation.

### Scheduling Advisory and Generation Boundaries

Scheduling analysis is intentionally limited to internal recommendations. The analyzer runs on the control runtime, reads windowed diagnostics, and logs advisory actions; it does not mutate the declared scheduling mode or request restarts in production. `SchedulingAdvisoryProfile` can disable advisory emission, but it does not change lifecycle, placement, reload, restart, or shutdown behavior.

A running Tokio future cannot be moved between runtimes. Future mode-internal placement work, such as HighPriority runtime epoch rollover, is deferred to later research and would need to happen at a generation boundary inside the same declared mode.

### 1.1. The Provider Change Signal Path
Provider reload propagation distinguishes value mutation from binding mutation:

```mermaid
sequenceDiagram
    participant Writer as Service/Test Writer
    participant Guard as TrackedWriteGuard
    participant Slot as Effective Provider Slot
    participant Scope as Daemon Provider Scope
    participant Watcher as ServiceWatcher
    participant Super as ServiceSupervisor

    Writer->>Guard: Mutate managed value
    Guard->>Slot: Publish value change on dirty drop/commit
    Writer->>Scope: Or install daemon-local override/fork
    Scope->>Watcher: Publish binding change
    Slot->>Watcher: Publish value change
    Watcher->>Super: Request Reload
    Super->>Super: Terminate Generation N
    Super->>Super: Spawn Generation N+1
```

- **Value mutation**: Managed providers publish through the effective slot's `StateManager`. Root slot changes reload daemons that still inherit root; daemon-local slot changes stay inside that daemon.
- **Binding mutation**: A daemon-local fork or simulation override changes which slot a provider type resolves to for that daemon. The binding epoch changes and dependent generations reload so the next generation resolves the new slot.
- **Dirty tracking**: Acquiring and releasing a write lock without mutating the value does not publish a value change.
- **Race Safety**: The `ServiceSupervisor` ensures that a reload only proceeds after the preceding generation has cleanly released its resources (e.g., ports, file handles).

### 1.2. Immediate Reloads
Even if a service is in a restart backoff delay (due to a failure), the `ServiceDaemon` remains reactive. If a **Reload Signal** is received (typically due to a dependency update), the daemon will interrupt the delay and restart the service immediately with the new configuration.

### 1.3. Fatal Errors
Fatal outcomes stop the current service generation without entering the retry/backoff loop.

- `ServiceError::Fatal(...)`: the supervisor marks that service as `Terminated` and does not restart it.
- `ProviderInitError` during lazy service startup: the supervisor treats this as a daemon-wide startup/runtime boundary failure, requests daemon shutdown, and terminates the affected service.
- Ordinary `Err(...)` and panics are different: they transition the service into `Recovering(...)` and restart with backoff.

A normal `Ok(())` return is also distinct from failure recovery. The supervisor still starts a fresh generation, but it does so immediately and records success on the backoff controller instead of counting the exit as another failure.

Trigger handlers run inside trigger service generations. A single handler `Err` is retried by the trigger runner; retry exhaustion and dispatch infrastructure errors are bridged back to the supervisor as recoverable generation failures. Dispatch task panics keep their panic classification, so panic lifecycle counters and backoff behavior remain consistent with ordinary service panics.

Isolated startup failures are intentionally not fatal. If an isolated OS thread, private Tokio runtime, or startup bridge cannot be created, the generation is classified as an isolated startup failure and restarted through the recoverable backoff path.

### 1.4. Generation Diagnostics

Each service generation is registered in an internal diagnostics store when the supervisor enters `Starting`. The generation records:

- statically declared body scheduling mode (`Standard`, `HighPriority`, or `Isolated`);
- lifecycle outcome classification (`NormalExit`, recoverable error, panic, fatal service error, provider init error, reload, shutdown, or isolated startup failure);
- reload requests, restart decisions, last restart decision kind, policy/effective restart delay, rate-limited restart flags, and termination;
- service-level `service_daemon::sleep()` completed/interrupted counts and wakeup drift;
- runtime heartbeat probe observations for the control plane and body execution lanes.

The supervisor includes a compact per-generation summary in the outcome tracing event. The public `DaemonDiagnosticsSnapshot` exposes distilled service, generation, and lane summaries through read-only daemon/handle methods. Generation-detail snapshot retention is bounded to the most recent 1024 generations per service so crash loops do not make snapshot collection and sorting unbounded; service and lane aggregates still accumulate across evicted generation details. Standard service and Standard lane summaries can include interpretation labels, confidence, and investigation hints, but those labels are derived from snapshot facts and do not change generation lifecycle, restart/backoff, reload, shutdown, or body placement. The store, windows, evaluator, recommendation fingerprints, thresholds, and mutation paths remain internal. In particular, isolated thread/runtime/bridge startup failures are classified separately, with a private startup failure kind, but still use the recoverable backoff path.

`last_exit_kind` and `last_restart_decision` are intentionally separate lifecycle facts. Clean exits and reloads record an immediate restart decision; recoverable service errors and trigger retry exhaustion record recoverable backoff; service or trigger dispatch panics record panic backoff; isolated startup failures record isolated-startup backoff. Fatal service errors, provider-init terminal errors, and shutdown exits do not synthesize a restart decision because they bypass the restart loop.

### 1.5. `BackoffController` Internals
The `BackoffController` is a stateful abstraction shared by both `ServiceSupervisor` and `TriggerRunner` (via `RetryInterceptor`). 

#### State Management
- **Delay Tracking**: Calculates `min(max_delay, initial_delay * multiplier^power)`.
- **Failure Count**: Incremented on every `Err` return; used as the `power` for calculation.
- **Signal Integration**: Integrates a `tokio::time::sleep` with the local `CancellationToken` or `watch::Receiver`, allowing immediate wake-up upon reload/shutdown.

#### Self-Healing Reset
The controller tracks the uptime of the current service generation. When a service remains in the `Healthy` state for longer than `reset_after` (default 60s), the failure counter is reset to 0. This prevents "historical baggage" from affecting the restart speed of stable systems.

#### Restart Storm Guard
Service supervisors layer an internal restart-storm guard on top of the policy backoff. Recoverable service errors, panics, and isolated startup failures are counted in a short sliding window; once the threshold is reached, the effective restart delay becomes `max(policy_delay, storm_guard_delay)`.

This guard is deliberately not a public `RestartPolicy` knob yet. Clean `Ok(())` exits and reloads reset both the backoff controller and the storm guard, fatal/provider-init outcomes bypass the guard, and shutdown still interrupts any restart wait immediately.

## 2. Wave-Based Orchestration

Services are started and stopped synchronized by waves of `priority`.

- **Startup (High to Low)**: Core services start first. A wave waits until services in it report `Healthy` (via a handshake), but only up to `wave_spawn_timeout`; once the timeout expires, the daemon logs a warning and continues with the next wave.
- **Shutdown (Low to High)**: External APIs stop first, followed by storage and then core systems.

## 3. The Handshake Protocol

A service indicates it is "ready" via a handshake. This prevents dependent services from starting before their prerequisites are fully initialized.

### Explicit Handshake
Calling `service_daemon::done()` manually. Recommended for complex initialization.

### Implicit Handshake
For minimalist services, any call to `is_shutdown()`, `sleep()`, or `wait_shutdown()` counts as a transition to `Healthy` if the service is still in an introductory phase (`Initializing`, `Restoring`, `Recovering`).

> [!TIP]
> **Implementation note**: The implicit handshake uses a task-local flag. Only the first lifecycle utility call per generation writes to the Status Plane; later calls use the cached flag and token checks.

## 4. State Persistence (The Shelf)

The "Shelf" is a daemon-scoped store where services can deposit data before a reload or after a crash.
- **Isolation**: Buckets are isolated by `ServiceId`, so two selected services with the same Rust function name cannot share shelf state accidentally.
- **Survival**: Unlike ordinary in-memory state, Shelf data survives generation termination and is inherited by the next generation of the same selected service.

## 5. Provider Initialization Errors

This section describes the error model for Providers whose initialization may fail (e.g., network binding, external configuration, credentials).

### 5.1. Fallible Providers

A Provider is considered **fallible** if its definition explicitly returns:

- `Result<T, ProviderError>`

> Important: Providers return the **plain** `T` value. The framework remains responsible for wrapping it in `Arc<T>` internally, consistent with the current provider design.

### 5.2. `ProviderError` Semantics

`ProviderError` is intended to be a public, extensible enum and must be marked `#[non_exhaustive]`.

The initial semantic surface area is intentionally small:

- `ProviderError::Fatal(...)`
  - **Immediate process exit** (strong fail-fast).
  - No retries.
- `ProviderError::Retryable(...)`
  - Retry with backoff according to `RestartPolicy`.
  - If retries exceed `provider_init_timeout`, exit the process.

#### FUTURE: Degraded providers

Future versions may extend `ProviderError` with additional semantics (e.g. a `Degraded` outcome), but this is **not implemented yet**.

If/when introduced, the following questions must be answered in the framework contract before enabling it:

- Does the daemon continue startup, and what is the readiness/health behaviour?
- How do services observe (and react to) the degraded state without pushing complexity into business code?

### 5.3. Lazy vs. Eager Provider Initialization

The default behavior is **lazy** for all providers (including `Listen`). Lazy initialization happens the first time a selected service, trigger, provider dependency, or helper call resolves that provider in its current effective scope.

A provider may opt into **eager** initialization via an explicit macro parameter:

- `#[provider(..., eager = true)]`

Eager initialization applies only to **reachable** providers (those referenced by the selected `Registry` services and their dependency graph), to avoid unnecessary work. During daemon startup, reachable eager providers resolve through that daemon's provider scope, not by bypassing the scoped bridge. By default the daemon inherits the root provider slot; simulation overrides or internal forks can install a daemon-local slot before eager initialization, so startup seeds the local slot without polluting root state.

### 5.4. RestartPolicy reuse

Provider initialization retries reuse the existing `RestartPolicy` model.

- `provider_init_timeout` is implemented today.
- By default, it matches `wave_spawn_timeout` to keep startup timing consistent unless explicitly configured.
- `ProviderError::Retryable(...)` keeps retrying until that timeout is reached.
- `ProviderError::Fatal(...)` skips retries and fails startup immediately.

### 5.5. Advanced provider helper and state preconditions

Generated provider helpers expose a low-level `resolve_managed()` path for tests, diagnostics, and framework integrations that need the raw `Result<Arc<T>, ProviderError>` before it is mapped into convenience initialization semantics. Normal services should prefer dependency injection and let the daemon own retries, fatal shutdown, and cancellation.

`StateManager::snapshot()` is a convenience API for already-initialized state. It panics if called before the corresponding provider has been initialized through the daemon/provider resolution path. That panic is intentional: pre-init snapshot probing is caller misuse, not a recoverable provider-init error.

### 5.6. Provider ownership boundary

Generated provider resolution uses a root scope plus daemon effective scopes. Calls made outside framework context fall back to the root scope, which preserves convenient helper usage in tests and setup code. Calls made while a daemon is starting or while a service, trigger, or watcher is running use that daemon's effective provider scope.

The effective binding is generation-sensitive: if a daemon-local binding changes, dependent services and triggers reload at a generation boundary. Existing generations are not silently mutated in place; the next generation resolves the new provider slot.

[Back to README](../../README.md)
