# Service Daemon

[![Rust CI](https://github.com/loft-games/service-daemon-rs/actions/workflows/rust.yml/badge.svg?branch=master)](https://github.com/loft-games/service-daemon-rs/actions/workflows/rust.yml)

`service-daemon-rs` is a sophisticated Rust framework for automatic service management and type-based dependency injection. Inspired by decorator-based registration in other languages, it brings seamless orchestration to the Tokio ecosystem.

## Key Features

- **Declarative Services**: Mark functions as managed tasks with `#[service]`.
- **Event-Driven Triggers**: Use `#[trigger]` for Cron, Queues, and State Watchers.
- **Type-Safe DI**: Dependency injection resolved at compile-time with zero boilerplate.
- **Eager Initialization**: Opt-in non-lazy startup via `eager = true` for all provider types (Struct, Fn, Templates).
- **Resilient Lifecycle**: Exponential backoff, jitter, wave-based startup/shutdown, and **fatal error handling**.
- **Early-Binding Listeners**: Use `#[provider(Listen("addr"))]` to bind ports at system-init, ensuring K8s/Knative readiness probes pass even while other services are still starting.
- **Smart State**: Transparent change tracking and zero-copy state snapshots.
- **Unified Params**: Consistent `env` and `capacity` support across all built-in template providers.
- **Isolated Unit Testing**: Feature-gated `MockContext` for injecting shadow Providers, Shelf, and Status with zero production overhead.
- **Tag-based Registry**: Filter services by tags for selective loading (`#[service(tags = ["infra"])]`).

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

Looking to build your first reliable background system? Follow our **[Grand Tour](docs/guide/tutorial/grand-tour.md)** tutorial series!

1. [**Hello, Heartbeat!**](docs/guide/tutorial/hello-heartbeat.md) - Your first background service. 
2. [**Reactive Triggers**](docs/guide/tutorial/reactive-triggers.md) - Events and automation.
3. [**The Art of Recovery**](docs/guide/tutorial/art-of-recovery.md) - State management and resilience.
4. [**Waves of Orchestration**](docs/guide/tutorial/orchestration-waves.md) - Startup and shutdown order.
5. ...and much more in the **[Full Tutorial](docs/guide/tutorial/grand-tour.md)**.

---

## Examples

The `examples/` directory contains focused examples organized by use case:

| Example | Focus | Run Command |
|:---|:---|:---|
| **minimal** | `is_shutdown()` polling -- simplest pattern | `cargo run -p example-minimal` |
| **complete** | `state()` lifecycle -- recovery, reload, priorities | `cargo run -p example-complete` |
| **triggers** | Decoupled event-driven handlers (Cron, Queue, Watch) | `cargo run -p example-triggers` |
| **logging** | File-based JSON log persistence (`file-logging` feature) | `cargo run -p example-logging` |
| **simulation** | `MockContext` for unit testing (`simulation` feature) | `cargo test -p example-simulation` |

> **Important**: Do NOT mix `is_shutdown()` polling (minimal) with `state()` lifecycle matching (complete) in the same service. These are two independent control-flow paradigms.

---

## Documentation

Our documentation is split by audience to ensure you find exactly what you need without the noise.

### User Guides (Framework Users)
*Everything you need to build and run your application.*

- [State Management](docs/guide/state-management.md): Providers, Mutability, and zero-copy snapshots.
- [Event Triggers](docs/guide/triggers.md): Cron, Queues, and Reactive Watchers.
- [Resilience & Lifecycle](docs/guide/resilience.md): Restarts, jitter, and wave-based orchestration.
- [Diagnostics & Logs](docs/guide/diagnostics.md): Using the `DaemonLayer` for real-time visibility.
- [Testing & Troubleshooting](docs/guide/testing-troubleshooting.md): Framework patterns, Mocking, and FAQ.

### Architecture & Internals (Core Developers)
*Deep dives into the technical "Why" and "How" of the engine.*

- [Internal Overview](docs/architecture/internal-overview.md): Registry design, linkme segments, and DI resolution.
- [The Ripple Model](docs/architecture/causal-tracing.md): Our unique philosophy for asynchronous causal tracing.
- [Lifecycle Deep Dive](docs/architecture/lifecycle-management.md): Reactive signal paths and supervisor internals.
- [Macros Mechanics](docs/architecture/macros-deep-dive.md): The magic behind attribute stripping and AST transformation.
- [Extending the Framework](docs/development/extending-framework.md): Guide for adding new trigger types or providers.

---

## License

Licensed under either of

- [MIT license](LICENSE-MIT)
- [Apache License, Version 2.0](LICENSE-APACHE)

at your option.
