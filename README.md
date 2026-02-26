# Service Daemon

[![Rust CI](https://github.com/loft-games/service-daemon-rs/actions/workflows/rust.yml/badge.svg?branch=master)](https://github.com/loft-games/service-daemon-rs/actions/workflows/rust.yml)

`service-daemon-rs` is a sophisticated Rust framework for automatic service management and type-based dependency injection. Inspired by decorator-based registration in other languages, it brings seamless orchestration to the Tokio ecosystem.

## Key Features

- **Declarative Services**: Mark functions as managed tasks with `#[service]`.
- **Event-Driven Triggers**: Use `#[trigger]` for Cron, Queues, and State Watchers.
- **Type-Safe DI**: Dependency injection resolved at compile-time with zero boilerplate.
- **Resilient Lifecycle**: Exponential backoff, jitter, wave-based startup/shutdown, and **fatal error handling**.
- **Smart State**: Transparent change tracking and zero-copy state snapshots.
- **Isolated Unit Testing**: Feature-gated `MockContext` for injecting shadow Providers, Shelf, and Status with zero production overhead.
- **Tag-based Registry**: Filter services by tags for selective loading (`#[service(tags = ["infra"])]`).

---

## Get Started (Read This First!)

New to `service-daemon-rs`? Avoid common traps and get your first service running in 5 minutes:

[**5-Minute Get Started Guide**](docs/guide/get-started.md)

---

## Quick Overview

`service-daemon-rs` allows you to build complex system daemons by simply annotating your components. 

To get your first service running safely and avoid common architectural pitfalls, please refer to our primary entry point:

[**5-Minute Get Started Guide**](docs/guide/get-started.md)

---

---

## Examples

The `examples/` directory contains focused examples organized by use case:

| Example | Focus | Run Command |
|:---|:---|:---|
| **minimal** | `is_shutdown()` polling — simplest pattern | `cargo run -p example-minimal` |
| **complete** | `state()` lifecycle — recovery, reload, priorities | `cargo run -p example-complete` |
| **triggers** | Decoupled event-driven handlers (Cron, Queue, Watch) | `cargo run -p example-triggers` |
| **logging** | File-based JSON log persistence (`file-logging` feature) | `cargo run -p example-logging` |
| **simulation** | `MockContext` for unit testing (`simulation` feature) | `cargo test -p example-simulation` |

> **Important**: Do NOT mix `is_shutdown()` polling (minimal) with `state()` lifecycle matching (complete) in the same service. These are two independent control-flow paradigms.

---

## Documentation Map

Explore our detailed documentation grouped by your needs:

### User Guides
Learn how to build applications with `service-daemon-rs`.
- [**5-Minute Get Started**](docs/guide/get-started.md): The absolute first place to look.
- [Provider Best Practices](docs/guide/provider-best-practices.md): Choosing between simple, async, and magic providers.
- [Concept Clarification & Pitfalls](docs/guide/pitfalls-faq.md): Avoiding common architectural traps and misconceptions.
- [Resilience & Lifecycle](docs/guide/resilience.md): Restarts, priorities, and shutdown.
- [Event Triggers](docs/guide/triggers.md): Cron, Queues, and Reactive Watchers.
- [State Management](docs/guide/state-management.md): Mutability, snapshots, and persistence.
- [Testing & Troubleshooting](docs/guide/testing-troubleshooting.md): Framework patterns and error resolution.

### Technical Reference
Deep dives into the internal mechanics.
- [Architecture Overview](docs/architecture/internal-overview.md): System flow and registry design.
- [Macros Deep Dive](docs/architecture/macros-deep-dive.md): The magic behind `#[service]` and tracked state.
- [Lifecycle & Status Plane](docs/architecture/lifecycle-management.md): Orchestration and state transitions.

### Contributor Guide
Help us improve the framework.
- [Contributing](docs/CONTRIBUTING.md): Environment setup and PR process.
- [Extending the Framework](docs/development/extending-framework.md): How to add new triggers or providers.

---

## License

MIT
