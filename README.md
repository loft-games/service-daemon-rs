# Service Daemon

[![Rust CI](https://github.com/loft-games/service-daemon-rs/actions/workflows/rust.yml/badge.svg?branch=master)](https://github.com/loft-games/service-daemon-rs/actions/workflows/rust.yml)

**The Declarative Engine for Resilient, Type-Safe Rust Microservices.**

`service-daemon-rs` is a lightweight framework that automates the orchestration of background services and event-driven triggers. By using compile-time registration, it eliminates boilerplate and ensures your system is resilient by design.

## Why choose service-daemon?

*   **Boilerplate-free Orchestration**: Define services and triggers with simple attributes like `#[service]` or `#[trigger(Cron("0 * * * *"))]`. No more manual wiring in `main`.
*   **Built-in Resilience**: production-ready patterns -- such as exponential backoff, jittered retries, and early-binding listeners for K8s readiness -- are baked in.
*   **Type-Safe Dependency Injection**: Resolve dependencies through Rust's type system. No runtime scanning, no reflection, and minimal-overhead discovery via linker-level integration (`linkme`).
*   **Visual Observability**: Automatically generate **Mermaid diagrams** of your service topology and track causal relationships across services with zero-allocation tracing.
*   **Testable by Design**: Includes a feature-gated `MockContext` that allows you to simulate complex async behaviors and state changes in a controlled sandbox.

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

Looking to build your first reliable background system? Follow our **[Quick Start Guide](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/quick-start.md)** tutorial series!

1. [**Hello, Heartbeat!**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/hello-heartbeat.md) - Your first background service. 
2. [**Reactive Triggers**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/reactive-triggers.md) - Events and automation.
3. [**State Management & Recovery**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/state-recovery.md) - Persistence and resilience.
4. [**Sequential Startup & Shutdown**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/priority-orchestration.md) - Priority-based orchestration.
5. ...and much more in the **[Full Guide](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/quick-start.md)**.

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
| **simulation** | `MockContext` for unit testing (`simulation` feature) | `cargo test -p example-simulation` |

> **Important**: Do NOT mix `is_shutdown()` polling (minimal) with `state()` lifecycle matching (complete) in the same service. These are two independent control-flow paradigms.

---

## Documentation

Our documentation is split by audience to ensure you find exactly what you need without the noise.

### User Guides (Framework Users)
*Everything you need to build and run your application.*

- [State Management](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/state-management.md): Providers, Mutability, and zero-copy snapshots.
- [Event Triggers](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/triggers.md): Cron, Queues, and Reactive Watchers.
- [Resilience & Lifecycle](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/resilience.md): Restarts, jitter, and wave-based orchestration.
- [Diagnostics & Logs](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/diagnostics.md): Using the `DaemonLayer` for real-time visibility.
- [Testing & Troubleshooting](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/testing-troubleshooting.md): Framework patterns, Mocking, and FAQ.

### Architecture & Internals (Core Developers)
*Deep dives into the technical "Why" and "How" of the engine.*

- [Internal Overview](https://github.com/loft-games/service-daemon-rs/blob/master/docs/architecture/internal-overview.md): Registry design, linkme segments, and DI resolution.
- [The Ripple Model](https://github.com/loft-games/service-daemon-rs/blob/master/docs/architecture/causal-tracing.md): Our unique philosophy for asynchronous causal tracing.
- [Lifecycle Deep Dive](https://github.com/loft-games/service-daemon-rs/blob/master/docs/architecture/lifecycle-management.md): Reactive signal paths and supervisor internals.
- [Macros Mechanics](https://github.com/loft-games/service-daemon-rs/blob/master/docs/architecture/macros-deep-dive.md): The magic behind attribute stripping and AST transformation.
- [Extending the Framework](https://github.com/loft-games/service-daemon-rs/blob/master/docs/development/extending-framework.md): Guide for adding new trigger types or providers.

---

## License

Licensed under either of

- [MIT license](https://github.com/loft-games/service-daemon-rs/blob/master/LICENSE-MIT)
- [Apache License, Version 2.0](https://github.com/loft-games/service-daemon-rs/blob/master/LICENSE-APACHE)

at your option.
