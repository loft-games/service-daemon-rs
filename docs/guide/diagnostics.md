# Diagnostics & The DaemonLayer

To manage complex asynchronous systems, visibility is paramount. `service-daemon-rs` provides a high-fidelity diagnostic layer built on top of `tracing`.

## 1. Entering the Matrix: `DaemonLayer`

The `DaemonLayer` is a specialized `tracing::Subscriber` layer that captures framework-level events that are usually too noisy for standard application logs.

### Enabling Diagnostics
In your `main.rs`, initialize the registry with the `DaemonLayer`:

```rust
use service_daemon::core::logging::DaemonLayer;
use tracing_subscriber::prelude::*;

fn main() {
    tracing_subscriber::registry()
        .with(DaemonLayer::default()) // Capture framework heartbeat
        .with(tracing_subscriber::fmt::layer()) // Standard logging
        .init();
    
    // ... start daemon ...
}
```

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
Every log message from a trigger handler is automatically tagged with:
- `msg_id`: The ID of the triggering event.
- `src_id`: The ID of the initiating service.
- `instance`: The generation of the running service.

## 3. Real-Time Instrumentation

The `service-daemon` also provides an internal `LogService` (available via the `Registry`) that can be used to stream these events to an external dashboard or a CLI tool.

| Field | Description |
| :--- | :--- |
| `System Pressure` | High-level saturate metric for the entire Wave. |
| `Wave Status` | Current active startup/shutdown wave. |
| `Panic Counts` | Persistent failure counters. |

[Back to README](../../README.md)
