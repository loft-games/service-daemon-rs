# Service Daemon

[![Rust CI](https://github.com/loft-games/service-daemon-rs/actions/workflows/rust.yml/badge.svg?branch=master)](https://github.com/loft-games/service-daemon-rs/actions/workflows/rust.yml)

**Manage the long-running loops in your tokio application.**

`service-daemon-rs` lets you declare the independent loops your program runs -- their dependencies, startup/shutdown order, failure recovery, and signal handling -- so your `main.rs` does not become a large block of hand-written `tokio::spawn` orchestration.

It earns its keep when your application has more than one long-running concern. Typical scenarios:

- **Protocol bridges and IoT gateways** -- HTTP <-> MQTT, WebSocket <-> Redis stream, multi-protocol device front ends.
- **Backends with rich background work** -- scheduled cleanup, cache warmers, queue consumers, webhook receivers, health monitors alongside your HTTP API.
- **Edge / robotics / industrial control** -- sensor pipelines, camera + inference + uplink, control loops with strict startup/shutdown ordering.
- **Any tokio application** where you find yourself rewriting the same `select! { ... shutdown ... } + restart + backoff` glue in every project.

## Why choose service-daemon?

*   **Declarative orchestration** -- Describe services, triggers, providers, and their relationships with attributes like `#[service]` or `#[trigger(Cron(CleanupSchedule))]`, where trigger targets are provider types. No manual service list or ad hoc spawn supervision in `main`.
*   **Production patterns built in** -- Exponential backoff with jitter, wave-based startup/shutdown by priority, scheduling lanes (`Standard`, `HighPriority`, `Isolated`), restart policies, graceful signal handling, early-binding TCP/Unix listeners -- the glue you'd otherwise rewrite per project.
*   **Type-safe dependency injection** -- Resolved by Rust's type system. No runtime container, no string keys, no reflection. Discovery is linker-level via `linkme`.
*   **Causal observability** -- UUID v7 message IDs propagate across services automatically. Optional **Mermaid** topology export visualizes the running system.
*   **Testable by design** -- A feature-gated `MockContext` lets you simulate async behavior and state transitions in a controlled sandbox without spinning up the full daemon.

## Quick Start

```rust
use service_daemon::prelude::*;
use service_daemon::{ServiceDaemon, provider, service, sleep};
use tracing::info;
use std::sync::Arc;

// 1. Define an injectable provider with a default value
#[derive(Clone)]
#[provider(8080)]
pub struct Port(pub i32);

// 2. Define a managed service using proc-macros
#[service]
pub async fn heartbeat_service(port: Arc<Port>) -> anyhow::Result<()> {
    while !is_shutdown() {
        info!("Heartbeat: service is alive on port {}", port);
        // Interruptible sleep: returns false if shutdown is requested
        if !sleep(std::time::Duration::from_secs(5)).await {
            break;
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 3. Build and run the daemon
    let mut daemon = ServiceDaemon::builder().build();
    daemon.run().await;
    daemon.wait().await?;
    Ok(())
}
```

## Get Started

The **[Quick Start Guide](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/quick-start.md)** walks through the framework one concept at a time:

1. [**First Service**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/first-service.md) -- Your first service.
2. [**Reactive Triggers**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/reactive-triggers.md) -- Events, queues, and chained handlers.
3. [**State Management & Recovery**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/state-recovery.md) -- Persistence across restarts.
4. [**Sequential Startup, Shutdown & Scheduling**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/priority-orchestration.md) -- Priority waves and runtime scheduling policies.
5. See the [**Full Guide**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/quick-start.md) for the complete chapter list.

---

## Examples

The `examples/` directory contains focused examples organized by use case:

| Example | Focus | Run Command |
|:---|:---|:---|
| **minimal** | `is_shutdown()` polling -- simplest pattern | `cargo run -p example-minimal` |
| **complete** | `state()` lifecycle -- recovery, reload, priorities | `cargo run -p example-complete` |
| **triggers** | Decoupled event-driven handlers (Cron, Queue, Watch) | `cargo run -p example-triggers` |
| **logging** | File-based JSON log persistence (`file-logging` feature) | `cargo run -p example-logging` |
| **diagnostics** | Behavioral Topology and Mermaid export (`diagnostics` feature) | `cargo run -p example-diagnostics` |
| **web-api** | Axum HTTP API with explicit CORS, OpenAPI docs, request envelopes, graceful shutdown, and maintenance triggers | `cargo run -p example-web-api` |
| **controller-bridge** | Simulated controller bridge: fake transport, framing, protobuf, bounded command correlation, custom `TriggerHost`, and status watch side effects | `cargo run -p example-controller-bridge` |
| **scheduling** | `Standard`, `HighPriority`, and `Isolated` runtime lanes for services | `cargo run -p examples-scheduling` |
| **unix-domain-socket** | Unix socket listener and connector pair | `cargo run -p example-unix-domain-socket` |
| **simulation** | `MockContext` for unit testing (`simulation` feature) | `cargo test -p example-simulation` |

> **Important**: Do NOT mix `is_shutdown()` polling (minimal) with `state()` lifecycle matching (complete) in the same service. These are two independent control-flow paradigms.

---

## Documentation

Documentation is split by audience.

### Tutorial
*Recommended first path for new users.*

- [Quick Start Tutorial](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/quick-start.md) -- Tutorial path from the first service through triggers, state recovery, retries, scheduling, and simulation.

### User Guides
*For people building applications on top of the framework who need complete usage references.*

- [State Management](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/state-management.md) -- Providers, mutability, zero-copy snapshots.
- [Event Triggers](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/triggers.md) -- Cron, queues, watchers.
- [Resilience & Lifecycle](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/resilience.md) -- Restart policy, jitter, wave-based orchestration.
- [Diagnostics & Logs](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/diagnostics.md) -- The `DaemonLayer` for runtime visibility.
- [Testing & Troubleshooting](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/testing-troubleshooting.md) -- Mocking, FAQ.

### Architecture & Internals
*For people studying framework design or debugging internals.*

- [Internal Overview](https://github.com/loft-games/service-daemon-rs/blob/master/docs/architecture/internal-overview.md) -- Registry design, linkme segments, DI resolution.
- [Causal Tracing](https://github.com/loft-games/service-daemon-rs/blob/master/docs/architecture/causal-tracing.md) -- Causal identity across asynchronous trigger chains.
- [Lifecycle Internals](https://github.com/loft-games/service-daemon-rs/blob/master/docs/architecture/lifecycle-management.md) -- Reload paths and supervisor internals.
- [Macro Expansion](https://github.com/loft-games/service-daemon-rs/blob/master/docs/architecture/macro-expansion.md) -- How `#[service]` / `#[trigger]` rewrite your code.

### Maintainer Notes
For contributors maintaining release validation and framework internals.

- [Release Validation](https://github.com/loft-games/service-daemon-rs/blob/master/docs/development/release-validation.md) -- Feature-to-test matrix, linkme platform smoke coverage, dependency baseline, and example layers.
- [Macro Attribute Normalization](https://github.com/loft-games/service-daemon-rs/blob/master/docs/development/macro-attribute-normalization.md) -- Maintainer contract for proc-macro parser cleanup without public syntax churn.

For contribution workflow and development notes, use the repository [Contributing tab](https://github.com/loft-games/service-daemon-rs?tab=contributing-ov-file).

---

## License

Licensed under either of

- [MIT license](https://github.com/loft-games/service-daemon-rs/blob/master/LICENSE-MIT)
- [Apache License, Version 2.0](https://github.com/loft-games/service-daemon-rs/blob/master/LICENSE-APACHE)

at your option.
