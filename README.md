# Service Daemon

[![Rust CI](https://github.com/loft-games/service-daemon-rs/actions/workflows/rust.yml/badge.svg?branch=master)](https://github.com/loft-games/service-daemon-rs/actions/workflows/rust.yml)

`service-daemon-rs` is a sophisticated Rust framework for automatic service management and type-based dependency injection. Inspired by decorator-based registration in other languages, it brings seamless orchestration to the Tokio ecosystem.

## Key Features

- **Declarative Services**: Mark functions as managed tasks with `#[service]`.
- **Event-Driven Triggers**: Use `#[trigger]` for Cron, Queues, and State Watchers.
- **Type-Safe DI**: Dependency injection resolved at compile-time with zero boilerplate.
- **Resilient Lifecycle**: Exponential backoff, jitter, and wave-based startup/shutdown.
- **Smart State**: Transparent change tracking and zero-copy state snapshots.

---

## Quick Start

### 1. Define a Provider
```rust
use service_daemon::provider;

#[provider(default = 8080)]
pub struct Port(pub i32);
```

### 2. Create a Service
```rust
use service_daemon::service;
use std::sync::Arc;

#[service]
pub async fn my_service(port: Arc<Port>) -> anyhow::Result<()> {
    while !service_daemon::is_shutdown() {
        service_daemon::sleep(std::time::Duration::from_secs(1)).await;
    }
    Ok(())
}
```

### 3. Run the Daemon
```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    service_daemon::ServiceDaemon::auto_init().run().await
}
```

---

## Documentation Map

Explore our detailed documentation grouped by your needs:

### 📖 User Guides
Learn how to build applications with `service-daemon-rs`.
- [Resilience & Lifecycle](docs/guide/resilience.md): Restarts, priorities, and shutdown.
- [Event Triggers](docs/guide/triggers.md): Cron, Queues, and Reactive Watchers.
- [State Management](docs/guide/state-management.md): Mutability, snapshots, and persistence.
- [Testing & Troubleshooting](docs/guide/testing-troubleshooting.md): Framework patterns and error resolution.

### ⚙️ Technical Reference
Deep dives into the internal mechanics.
- [Architecture Overview](docs/architecture/internal-overview.md): System flow and registry design.
- [Macros Deep Dive](docs/architecture/macros-deep-dive.md): The magic behind `#[service]` and tracked state.
- [Lifecycle & Status Plane](docs/architecture/lifecycle-management.md): Orchestration and state transitions.

### 🛠️ Contributor Guide
Help us improve the framework.
- [Contributing](docs/development/contributing.md): Environment setup and PR process.
- [Extending the Framework](docs/development/extending-framework.md): How to add new triggers or providers.

---

## License

MIT
