# Diagnostics & The DaemonLayer

To manage complex asynchronous systems, visibility is paramount. `service-daemon-rs` provides a high-fidelity diagnostic layer built on top of `tracing`.

## 1. Entering the Matrix: `DaemonLayer`

The `DaemonLayer` is a specialized `tracing::Layer` that captures **all** tracing events, extracts business IDs from the current Span context, and pushes structured `LogEvent` instances to a non-blocking broadcast queue (capacity: 65,536). Two independent SYSTEM-priority consumers process this queue:

- **`log_service`** (tag: `__log__`): Renders events to stderr with ANSI colors.
- **`file_log_service`** (tag: `__file_log__`, feature-gated: `file-logging`): Persists events as JSON lines to daily-rotating log files.

Both consumers use a **fill-the-valley** batch strategy with a safety cap of 1,024 events per drain cycle. They are independent broadcast subscribers — failure in one does not affect the other.

### Enabling Diagnostics

**Standard initialization** (recommended for all binaries):

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Registers DaemonLayer + EnvFilter (reads RUST_LOG, defaults to "info")
    service_daemon::core::logging::init_logging();

    let mut daemon = ServiceDaemon::builder().build();
    daemon.run().await;
    daemon.wait().await
}
```

**Test environments** — use `try_init_logging()` to handle parallel test races:

```rust
#[tokio::test]
async fn my_test() {
    let _ = service_daemon::core::logging::try_init_logging();
    // ... test logic
}
```

**Custom subscriber stacks** (Sentry, OpenTelemetry, etc.) — use `DaemonLayer` directly:

```rust
use service_daemon::core::logging::DaemonLayer;
use tracing_subscriber::prelude::*;

tracing_subscriber::registry()
    .with(tracing_subscriber::EnvFilter::new("debug"))
    .with(DaemonLayer)
    .with(my_sentry_layer)
    .init();
```

**File logging** — configured independently:

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

> **Warning**: Do **not** add `tracing_subscriber::fmt::layer()` alongside `DaemonLayer`. The `log_service` handles all console output — adding `fmt::layer()` causes duplicate lines.

## 2. What to Look For

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
- **`service_id`**: The `ServiceId` of the service that produced the event.
- **`message_id`**: The globally unique ID of the triggering event (trigger context only).
- **`source_id`**: The `ServiceId` of the service that originally published the trigger event.

These IDs are `None` for log events outside a service context (e.g., daemon initialization).

## 3. Real-Time Instrumentation

The `log_service` and `file_log_service` are SYSTEM-priority background services that independently consume the `LogQueue` broadcast channel using a **fill-the-valley** batch strategy: each greedily drains all available events via `try_recv()` (up to a safety cap of 1,024) before flushing the batch in a single pass. This minimizes lock contention under high throughput while maintaining low latency under normal load.

In simulation environments, `MockContext` automatically includes `log_service` via infrastructure tag injection. This can be disabled with `.with_logging(false)` for lightweight tests.

| Field | Description |
| :--- | :--- |
| `System Pressure` | High-level saturate metric for the entire Wave. |
| `Wave Status` | Current active startup/shutdown wave. |
| `Panic Counts` | Persistent failure counters. |

[Back to README](../../README.md)
