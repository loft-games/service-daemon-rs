# Diagnostics and DaemonLayer

`service-daemon-rs` includes a diagnostic layer built on top of `tracing`. It extracts framework IDs from spans and writes structured log events for console output, file logging, and optional topology diagnostics. For the underlying design, see **[Architecture Overview](../architecture/internal-overview.md)**.

## 1. `DaemonLayer` pipeline

`DaemonLayer` is a `tracing::Layer` that captures tracing events, extracts service and trigger IDs from the current span context, and pushes structured `LogEvent` instances to a non-blocking broadcast queue. The queue capacity is derived as `batch_size * 4` (default: 128 * 4 = 512 slots; configurable via `set_log_batch_size()`, up to `MAX_LOG_BATCH_SIZE`). Two independent SYSTEM-priority consumers process this queue:

- **`log_service`** (tag: `__log__`): Renders events to stderr with ANSI colors.
- **`file_log_service`** (tag: `__file_log__`, feature-gated: `file-logging`): Persists events as JSON lines to daily-rotating log files.
- **`topology_collector`** (feature-gated: `diagnostics`): Aggregates causal edges between services for real-time behavioral mapping.

Both logging consumers drain events in batches, with a safety cap of 1,024 events per drain cycle. They are independent broadcast subscribers - failure in one does not affect the other.

### Enabling Diagnostics

**Standard initialization** (recommended for all binaries):

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Registers DaemonLayer + EnvFilter (reads RUST_LOG, defaults to "info")
    service_daemon::init_logging();

    let daemon = ServiceDaemon::builder().build();
    daemon.run().await;
    daemon.wait().await?;

    Ok(())
}
```

**Test environments** - use `try_init_logging()` to handle parallel test races:

```rust
#[tokio::test]
async fn my_test() {
    let _ = service_daemon::try_init_logging();
    // ... test logic
}
```

**Custom subscriber stacks** (Sentry, OpenTelemetry, etc.) - use `DaemonLayer` directly:

```rust
use service_daemon::DaemonLayer;
use tracing_subscriber::prelude::*;

tracing_subscriber::registry()
    .with(tracing_subscriber::EnvFilter::new("debug"))
    .with(DaemonLayer)
    .with(my_sentry_layer)
    .init();
```

**File logging** - configured independently:

```rust
use service_daemon::{FileLogConfig, enable_file_logging};

// Daily rotation, retains last 30 log files (defaults)
enable_file_logging(FileLogConfig::new("logs", "my-app"));
```

If the file appender cannot be initialized, for example because the configured
path is not writable or is not a directory, the file logging service logs a
warning and degrades to console logging only. It does not fail daemon startup
and does not hot-restart the SYSTEM logging service. Deployments that require
audit-grade file persistence should validate the log directory with an external
startup check or operational probe.

Custom rotation and retention can be configured via the struct fields:

```rust
use service_daemon::{FileLogConfig, RotationPolicy, enable_file_logging};

let config = FileLogConfig {
    rotation: RotationPolicy::Hourly,
    max_log_files: Some(48), // keep last 48 hourly files (2 days)
    ..FileLogConfig::new("logs", "my-app")
};
enable_file_logging(config);
```

**Log batch size** - controls both drain cycle size and queue capacity:

```rust
use std::num::NonZeroUsize;

use service_daemon::set_log_batch_size;

// Reduce batch size for a lightweight embedded daemon.
// Must be called BEFORE init_logging().
if let Some(batch_size) = NonZeroUsize::new(512) {
    if let Err(error) = set_log_batch_size(batch_size) {
        eprintln!("log batch size was not applied: {error}");
    }
}
service_daemon::init_logging();
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

On daemon shutdown, the framework also emits any collected topology through a
`tracing::info!` event with a `topology_mermaid` field. It does not write the
automatic export directly to stdout; the configured subscriber, console logger,
or file logger decides where that event is rendered. The topology can reveal
service names and causal relationships, so route it through the same log controls
as other diagnostics.

This is particularly useful for debugging complex "cascading" triggers where one event leads to a chain of reactions.

## 3. Runtime Pressure Baseline

The daemon keeps a runtime-pressure baseline for diagnostics. Public APIs expose a distilled read-only snapshot from that baseline, while the store, windows, evaluator, and recommendation model remain private.

Two low-level signals feed this baseline:

- **Service sleep drift**: `service_daemon::sleep(duration)` records `requested`, `elapsed`, and `drift = elapsed - requested` when the sleep completes. Reload and shutdown interruptions are counted separately and do not contribute drift.
- **Runtime heartbeat probes**: the framework runs low-frequency internal sleep probes on the daemon-owned Control runtime, on the host Standard body runtime, on the daemon-owned HighPriority body runtime when it exists, and inside each Isolated private runtime generation.

The logical lanes are reported separately as `Control`, `Standard`, `HighPriority`, and `Isolated`. `Control` is an internal diagnostics/control-plane lane, not a user-facing `ServiceScheduling` option. `Standard` represents the host runtime body lane rather than the supervisor/control runtime.

HighPriority runtime capacity is derived from the daemon's final declared `HighPriority` service/trigger entries before the runtime is lazily created. The resulting worker count is read-only operational context: pressure probes and advisory recommendations can warn about lane saturation, but they do not resize the worker count, reload services, restart services, trigger rollover, or move work across modes.

Generation outcome logs include a compact summary of sleep/probe observations, restart decisions, policy/effective restart delay, rate-limited restart state, termination, and the internal exit classification. Sleep drift is a wakeup-delay signal: it can be caused by executor pressure, OS scheduling, blocking tasks, I/O wake storms, or test-host load. It is not a CPU profiler and it does not trigger automatic migration or rescheduling.

### Public Diagnostics Snapshot

`DaemonInstanceHandle::diagnostics_snapshot()` returns a `DaemonDiagnosticsSnapshot`. The snapshot contains service summaries, generation summaries, and logical lane summaries.

- Service and generation records expose `declared_scheduling`, the static `ServiceScheduling` declaration generated by `#[service]` or `#[trigger]`.
- Generation-detail records are bounded to the most recent 1024 generations per service; service and lane aggregates continue accumulating across evicted generation details.
- Lane records expose `DiagnosticRuntimeLane`, including the internal `Control` diagnostics lane for observation-only aggregates.
- Observation stats expose completed/interrupted counts plus total/avg/max/last drift in milliseconds.
- Lifecycle stats expose reload, restart, backoff, rate-limited restart, termination, exit-kind counters, last exit kind, last policy/effective restart delay, and last restart decision.
- Shutdown boundary stats expose bounded shutdown outcomes for isolated runtime join and trigger dispatch drain, including completed/timed-out/panicked counters, residual work, and the last boundary outcome/action.
- Provider failure stats expose provider-init terminal failures in service, generation, and lane aggregates, including failure kind counters, source kind counters, runtime phase counters, resolve boundary counters, retry diagnostics presence, and last observed provider failure facts.
- Daemon-level snapshots retain a bounded recent `provider_failures` list with provider name, runtime phase, resolve boundary, source kind, failure kind, error summary, and retry diagnostics when available.
- Trigger services use the same lifecycle counters: retry exhaustion and dispatch infrastructure errors appear as recoverable exits, while dispatch panics appear as panic exits.
- Standard service and Standard lane records may include read-only `DiagnosticInterpretation` entries with a label, confidence, and investigation hints.
- Snapshot reads are side-effect free: they do not emit advisory logs, mutate the diagnostics store, reload/restart services, or change body placement.

The public snapshot is a distilled read model. It does not expose `DiagnosticsStore`, diagnostics windows, recommendation fingerprints, evaluator thresholds, or mutation paths.

### Runtime Facts and Readiness Snapshots

Use runtime facts for health endpoints, debug pages, or operational checks that
need current daemon state:

```rust
let runtime = daemon.runtime();
let readiness = daemon.runtime_readiness();
let services = daemon.runtime_services();
let trigger = trigger_instance.trigger_runtime();
```

`DaemonRuntimeSnapshot` reports daemon identity, uptime, shutdown state, and
registered service/trigger counts. `ServiceRuntimeSnapshot` reports service
identity, declared scheduling, lifecycle status, generation, restart count, and
recent lifecycle timing/error facts. `ReadinessSnapshot` groups services by
status and carries recent errors. It does not compute an `is_ready` or degraded
verdict; applications map the grouped facts to their own readiness contract.

Trigger handlers can read self-scoped pressure without receiving a daemon-wide handle:

```rust
async fn on_event(ctx: TriggerContext<MyEvent>) -> anyhow::Result<()> {
    if let Some(pressure) = ctx.pressure()
        && pressure.in_flight >= pressure.current_limit
    {
        // Application code may skip optional work based on read-only facts.
    }
    Ok(())
}
```

Trigger runtime snapshots include `in_flight`, `current_limit`,
`available_permits`, dispatch/retry counters, recent success/error timestamps,
and optional streaming pressure counters. They do not include application
payloads, private application keys, or runtime handles.

Runtime snapshots are read-only. A trigger that wants to react to pressure can
submit a temporary overlay with
`TriggerContext::request_policy_overlay(...)`; see
[Queue concurrency](triggers.md#7-queue-concurrency-async-dispatch).

### Lifecycle Facts and Restart Decisions

`last_exit_kind` describes why the previous recorded generation ended. `last_restart_decision` describes the most recent restart path the supervisor actually entered. Keeping these facts separate avoids making users infer restart meaning from delay values alone.

- Service generations that return `Ok(())` without a shutdown or reload control signal record `DiagnosticGenerationExitKind::NormalExit` and `DiagnosticRestartDecisionKind::BackoffNormalExit`.
- Reload restarts record `DiagnosticRestartDecisionKind::Immediate`.
- Recoverable service errors, trigger retry exhaustion, and trigger dispatch infrastructure failures record `BackoffRecoverableError`.
- Service panics and trigger dispatch panics record `BackoffPanic`.
- Isolated thread/runtime/bridge startup failures record `BackoffIsolatedStartupFailure`.
- Internal supervisor consistency failures record `BackoffInternalSupervisorError`.
- Shutdown, fatal service errors, and provider-init terminal errors do not create a synthetic restart decision.

Provider-init terminal errors remain coarse in lifecycle/status summaries (`ProviderInitError` as the lifecycle exit kind), while provider failure diagnostics are available as structured snapshot facts. They include runtime phase, generated wrapper boundary, typed source kind, failure kind, and retry-timeout details (`attempts`, elapsed time, last delay, and a bounded recent-error list). Those fields are diagnostic facts only: they do not expose retry controls, mutate providers, or change daemon shutdown behavior.

These fields are observation facts. They do not request a restart, override `RestartPolicy`, or make recommendations executable.

### Snapshot-to-Exporter Boundary

Metrics exporters should be thin adapters over `DaemonDiagnosticsSnapshot`: read the snapshot, map typed fields to vendor names/units/labels, and publish without mutating daemon state. The core runtime intentionally does not ship a Prometheus or OpenTelemetry schema here.

Exporter adapters should treat `#[non_exhaustive]` diagnostics enums defensively, control label cardinality, and account for bounded generation-detail retention. Service and lane aggregates are suitable for cumulative export; generation-level export should be understood as a recent bounded view rather than an infinite event log.

### Read-only Diagnostic Interpretations

The snapshot includes a small interpretation layer on top of the raw counters. It is meant to help humans read Standard runtime symptoms, not to identify a definitive culprit or issue commands.

Interpretation labels include low-sample suppression, host-runtime wake-delay suspicion, service-local wake-delay suspicion, service impacted by Standard lane pressure, blocking-risk suspicion, wake-storm suspicion, and lifecycle instability. Each interpretation carries `DiagnosticConfidence` and `DiagnosticRecommendationHint` values such as continue observing, investigate the host runtime, check blocking work, add business tracing, consider changing the source-level declared mode in a later code change, or investigate lifecycle instability first.

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

The analyzer does not expose a public metrics schema, does not change the declared service scheduling mode, and does not feed HighPriority worker-count selection. Mode-internal placement changes, such as HighPriority runtime epoch rollover, are outside the current runtime contract; if added, they must happen at a generation boundary inside the same declared mode.

The default `SchedulingAdvisoryProfile` keeps this advisory loop enabled. To suppress advisory emission without changing lifecycle or body placement, configure the daemon explicitly:

```rust,ignore
use service_daemon::{SchedulingAdvisoryProfile, ServiceDaemon};

let daemon = ServiceDaemon::builder()
    .with_scheduling_advisory_profile(SchedulingAdvisoryProfile::disabled())
    .build();
```

### Restart and Recovery Signals

When a service generation restarts after a recoverable failure, structured logs include the failure kind, restart decision kind, configured policy delay, effective restart delay, whether the internal storm guard extended the delay, and the number of failures currently visible in the storm window. Lifecycle snapshots track the same last restart decision plus rate-limited restart counts and the last policy/effective delay pair.

For triggers, retry exhaustion and dispatch infrastructure failures use those same restart/recovery signals. A dispatch panic is classified as a panic exit and a panic backoff restart decision, so the `panic` counter, `last_exit_kind`, and `last_restart_decision` distinguish it from ordinary recoverable exhaustion.

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

- **`service_instance_id`**: The `ServiceInstanceId` of the service instance that produced the event.
- **`source_service_instance_id`**: The `ServiceInstanceId` of the service instance that originally emitted the event, when available.
- **`message_id`**: The globally unique **UUID v7** (time-ordered) of the event that triggered this handler.
- **`trigger_instance_id`**: A composite identifier (`svcinst#<uuid>:<seq>`) that uniquely identifies this trigger invocation generation.

These IDs are `None` for log events outside a service context (e.g., daemon initialization).

- `Panic Counts`: Persistent failure counters.

[Back to README](../../README.md)
