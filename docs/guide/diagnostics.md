# Visual Observability & The DaemonLayer

To manage complex asynchronous systems, visibility is paramount. `service-daemon-rs` provides a high-fidelity diagnostic layer built on top of `tracing`. For the underlying high-performance design philosophy (Zero-allocation, context extraction), see **[Architecture Overview](../architecture/internal-overview.md)**.

## 1. Entering the Matrix: `DaemonLayer`

The `DaemonLayer` is a specialized `tracing::Layer` that captures **all** tracing events, extracts business IDs from the current Span context, and pushes structured `LogEvent` instances to a non-blocking broadcast queue. The queue capacity is automatically derived as `batch_size * 4` (default: 128 * 4 = 512 slots; configurable via `set_log_batch_size()`). Two independent SYSTEM-priority consumers process this queue:

- **`log_service`** (tag: `__log__`): Renders events to stderr with ANSI colors.
- **`file_log_service`** (tag: `__file_log__`, feature-gated: `file-logging`): Persists events as JSON lines to daily-rotating log files.
- **`topology_collector`** (feature-gated: `diagnostics`): Aggregates causal edges between services for real-time behavioral mapping.

Both logging consumers use a **fill-the-valley** batch strategy with a safety cap of 1,024 events per drain cycle. They are independent broadcast subscribers - failure in one does not affect the other.

### Enabling Diagnostics

**Standard initialization** (recommended for all binaries):

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Registers DaemonLayer + EnvFilter (reads RUST_LOG, defaults to "info")
    service_daemon::core::logging::init_logging();

    let mut daemon = ServiceDaemon::builder().build();
    daemon.run().await;
    daemon.wait().await?;

    Ok(())
}
```

**Test environments** - use `try_init_logging()` to handle parallel test races:

```rust
#[tokio::test]
async fn my_test() {
    let _ = service_daemon::core::logging::try_init_logging();
    // ... test logic
}
```

**Custom subscriber stacks** (Sentry, OpenTelemetry, etc.) - use `DaemonLayer` directly:

```rust
use service_daemon::core::logging::DaemonLayer;
use tracing_subscriber::prelude::*;

tracing_subscriber::registry()
    .with(tracing_subscriber::EnvFilter::new("debug"))
    .with(DaemonLayer)
    .with(my_sentry_layer)
    .init();
```

**File logging** - configured independently:

```rust
use service_daemon::core::logging::{FileLogConfig, enable_file_logging};

// Daily rotation, retains last 30 log files (defaults)
enable_file_logging(FileLogConfig::new("logs", "my-app"));
```

Custom rotation and retention can be configured via the struct fields:

```rust
use service_daemon::core::logging::{FileLogConfig, RotationPolicy, enable_file_logging};

let config = FileLogConfig {
    rotation: RotationPolicy::Hourly,
    max_log_files: Some(48), // keep last 48 hourly files (2 days)
    ..FileLogConfig::new("logs", "my-app")
};
enable_file_logging(config);
```

**Log batch size** - controls both drain cycle size and queue capacity:

```rust
use service_daemon::set_log_batch_size;

// Reduce batch size for a lightweight embedded daemon
// Queue capacity will be 512 * 4 = 2,048 slots
set_log_batch_size(512);
// Must be called BEFORE init_logging()
service_daemon::core::logging::init_logging();
```

## 2. Behavioral Topology (`diagnostics` feature)

When the `diagnostics` feature is enabled, you can activate the background topology collector. It observes message correlation across the system to build a live map of service interactions.

```rust
use service_daemon::{start_topology_collector, export_mermaid};

// 1. Start the background collector
start_topology_collector();

// ... run your daemon ...

// 2. Export the collected topology as a Mermaid diagram
if let Some(mermaid) = export_mermaid() {
    println!("System Topology:\n{}", mermaid);
}
```

This is particularly useful for debugging complex "cascading" triggers where one event leads to a chain of reactions.

## 3. Runtime Pressure Baseline

The daemon keeps a runtime-pressure baseline for diagnostics. Phase 6 exposes a distilled read-only snapshot from that baseline while keeping the internal store, windows, evaluator, and recommendation model private.

Two low-level signals feed this baseline:

- **Service sleep drift**: `service_daemon::sleep(duration)` records `requested`, `elapsed`, and `drift = elapsed - requested` when the sleep completes. Reload and shutdown interruptions are counted separately and do not contribute drift.
- **Runtime heartbeat probes**: the framework runs low-frequency internal sleep probes on the daemon-owned Control runtime, on the host Standard body runtime, on the daemon-owned HighPriority body runtime when it exists, and inside each Isolated private runtime generation.

The logical lanes are reported separately as `Control`, `Standard`, `HighPriority`, and `Isolated`. `Control` is an internal diagnostics/control-plane lane, not a user-facing `ServiceScheduling` option. `Standard` represents the host runtime body lane rather than the supervisor/control runtime.

HighPriority runtime capacity is planned from the daemon's final declared `HighPriority` service/trigger entries before the runtime is lazily created. The plan is read-only operational context: pressure probes and advisory recommendations can warn about lane saturation, but they do not resize the worker count, reload services, restart services, trigger rollover, or move work across modes.

Generation outcome logs include a compact summary of sleep/probe observations, restart decisions, policy/effective restart delay, rate-limited restart state, termination, and the internal exit classification. Sleep drift is a wakeup-delay signal: it can be caused by executor pressure, OS scheduling, blocking tasks, I/O wake storms, or test-host load. It is not a CPU profiler and it does not trigger automatic migration or rescheduling.

### Public Diagnostics Snapshot

`ServiceDaemon::diagnostics_snapshot()` and `ServiceDaemonHandle::diagnostics_snapshot()` return a `DaemonDiagnosticsSnapshot`. The snapshot contains service summaries, generation summaries, and logical lane summaries.

- Service and generation records expose `declared_scheduling`, the static `ServiceScheduling` declaration generated by `#[service]` or `#[trigger]`.
- Lane records expose `DiagnosticRuntimeLane`, including the internal `Control` diagnostics lane for observation-only aggregates.
- Observation stats expose completed/interrupted counts plus total/avg/max/last drift in milliseconds.
- Lifecycle stats expose reload, restart, backoff, rate-limited restart, termination, and exit-kind counters.
- Standard service and Standard lane records may include read-only `DiagnosticInterpretation` entries with a label, confidence, and investigation hints.
- Snapshot reads are side-effect free: they do not emit advisory logs, mutate the diagnostics store, reload/restart services, or change body placement.

The public snapshot is a distilled read model. It does not expose `DiagnosticsStore`, diagnostics windows, recommendation fingerprints, evaluator thresholds, or mutation paths.

### Read-only Diagnostic Interpretations

Phase 8 adds a small interpretation layer on top of the raw counters. It is meant to help humans read Standard runtime symptoms, not to identify a definitive culprit or issue commands.

Interpretation labels include low-sample suppression, host-runtime wake-delay suspicion, service-local wake-delay suspicion, service impacted by Standard lane pressure, blocking-risk suspicion, wake-storm suspicion, and lifecycle instability. Each interpretation carries `DiagnosticConfidence` and `DiagnosticRecommendationHint` values such as continue observing, investigate the host runtime, check blocking work, add business tracing, consider changing the source-level declared mode in a future build, or investigate lifecycle instability first.

The Standard lane uses runtime heartbeat probe drift to label host-runtime wake delay and wake-storm symptoms. Declared Standard services use their own `service_daemon::sleep()` drift and lifecycle counters to label service-local wake delay, Standard lane impact, blocking risk, wake storm, or lifecycle instability. Lifecycle instability takes precedence over placement-like hints because repeated restarts, panics, fatal errors, or provider init failures are stronger signals than wake-delay interpretation.

These interpretations are still read-only snapshot metadata. They do not read or drive the adaptive scheduling recommendation loop, do not expose public thresholds/windows/samplers, do not configure the host Tokio runtime, and do not trigger reload, restart, remap, worker-count changes, or mode migration.

### Scheduling Advisory Recommendations

The daemon also runs a low-frequency internal analyzer on the control runtime. It samples the diagnostics store into short windows, suppresses low-sample observations, and emits structured `tracing` recommendations only when the recommendation fingerprint changes.

These recommendations are advisory. They can report:

- Standard lane pressure when runtime probe drift is sustained.
- An impacted Standard service when service-level sleep drift is high, without declaring that service to be the culprit.
- Control-plane pressure as an investigation signal, never as a body migration command.
- HighPriority saturation as a warning, not as permission to move more services into HighPriority.
- Isolated resource pressure when isolated startup failures or rate-limited restarts appear.
- Lifecycle instability when restart/backoff signals are high, which suppresses placement-like advice.

The analyzer does not expose a public metrics schema and does not change the declared service scheduling mode. It is not an input to Phase 7 HighPriority worker-count planning. Future mode-internal runtime placement work, such as HighPriority runtime epoch rollover, is deferred to Phase 9 research and would need to be a generation-boundary decision inside the same declared mode.

The default `SchedulingAdvisoryProfile` keeps this advisory loop enabled. To suppress advisory emission without changing lifecycle or body placement, configure the daemon explicitly:

```rust,ignore
use service_daemon::{SchedulingAdvisoryProfile, ServiceDaemon};

let mut daemon = ServiceDaemon::builder()
    .with_scheduling_advisory_profile(SchedulingAdvisoryProfile::disabled())
    .build();
```

### Restart and Recovery Signals

When a service generation restarts after a recoverable failure, structured logs include the failure kind, configured policy delay, effective restart delay, whether the internal storm guard extended the delay, and the number of failures currently visible in the storm window. Internal lifecycle snapshots also track rate-limited restart counts and the last policy/effective delay pair.

Log fields remain diagnostic only. They do not change trigger retry semantics or expose internal recommendation state beyond the public snapshot read model.

## 4. What to Look For

> [!WARNING]
> Do **not** add `tracing_subscriber::fmt::layer()` alongside `DaemonLayer`.
> 1. **Duplication**: The `log_service` already handles console output -- adding `fmt::layer()` will cause every log line to appear twice.
> 2. **Performance (Blocking)**: `fmt::layer()` is synchronous and can block the async runtime under heavy load. `DaemonLayer` is fully asynchronous, offloading output to the managed `log_service` with internal batching to ensure zero-latency logging even during bursts.

Once enabled, you will see structured diagnostic signals in your logs:

### Service Transitions

Logs will include the exact millisecond a service moves between states written through the shared lifecycle plane:

- `Initializing -> Healthy`: Startup handshake successful.
- `NeedReload -> Terminated`: The reloading generation acknowledged cleanup and exited.

For reloads specifically, user code may observe `state() == NeedReload` as soon as the supervisor cancels the generation's reload token. That service-local observation can happen before a separate `Healthy -> NeedReload` write appears in the shared status map.

### Scaling & Pressure Metrics

For triggers with elastic scaling, `DaemonLayer` reports:

- **`current_limit`**: The current concurrency semaphore size.
- **`pressure_ratio`**: A decimal representing how saturated the trigger is.
- **`shadow_permits`**: When scaling down, this shows how many permits are currently ignored by the runner.

### Causal Correlation IDs

Every log event inside a service or trigger Span is automatically tagged with:

- **`service_id`**: The name of the service that produced the event (e.g., `"my-service"`).
- **`message_id`**: The globally unique **UUID v7** (time-ordered) of the event that triggered this handler.
- **`instance_id`**: A numeric composite identifier (e.g., `svc#1:42`) that uniquely identifies this trigger invocation generation.

These IDs are `None` for log events outside a service context (e.g., daemon initialization).

- `Panic Counts`: Persistent failure counters.

[Back to README](../../README.md)
