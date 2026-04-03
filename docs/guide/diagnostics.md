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

## 3. What to Look For

> [!WARNING]
> Do **not** add `tracing_subscriber::fmt::layer()` alongside `DaemonLayer`.
> 1. **Duplication**: The `log_service` already handles console output -- adding `fmt::layer()` will cause every log line to appear twice.
> 2. **Performance (Blocking)**: `fmt::layer()` is synchronous and can block the async runtime under heavy load. `DaemonLayer` is fully asynchronous, offloading output to the managed `log_service` with internal batching to ensure zero-latency logging even during bursts.

Once enabled, you will see structured diagnostic signals in your logs:

### Service Transitions
Logs will include the exact millisecond a service moves between states:
- `Initializing -> Healthy`: Startup handshake successful.
- `Healthy -> NeedReload`: Dependency change detected.
- `NeedReload -> Terminated`: Service generation cleanup started.

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
