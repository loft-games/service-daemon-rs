# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0-alpha.3] - 2026-03-29

### Added

- **Service Scheduling Policies**: Introduced `Standard`, `HighPriority`, and `Isolated` execution modes for fine-grained service lifecycle control.
- **Causal Tracing Identity**: Formalized task identity as a 4-tuple for precise asynchronous tracing.
- **Improved Visibility for Macros**: Enhanced support for `pub(super)` and restricted visibility in `#[service]` and `#[trigger]` expansions.

### Changed

- **Refactored Macro Codegen**: Unified internal code generation helpers for better maintainability and robustness.
- **Optimized Health Checks**: Refined service startup health verification and shutdown signaling.
- **CI/CD Enhancements**: Upgraded GitHub Actions (v6) and Node environment (v24).

### Fixed

- **Instance Interference**: Resolved an issue where multiple `ServiceDaemon` instances could interfere via sub-token collision.
- **Graceful Shutdown Integration**: Fixed compatibility with `axum::serve().with_graceful_shutdown()` patterns.
- **Tracing Span Extraction**: Standardized message identity capture and link propagation across dispatch tasks.

## [0.1.0-alpha.2] - 2026-03-14

### Added

- **Eager Initialization**: Added `eager = true` parameter support for all `#[provider]` types, ensuring critical resources are ready before service startup.
- **Resilient Providers**: Integrated native support for fallible initialization with `ProviderError` and `RestartPolicy` coordination.
- **Listen Template**: New `Listen` provider template for early-binding TCP listeners with file descriptor cloning support.

### Changed

- **DI Architecture Overhaul**: Refactored `ManagedState` to support zero-cost resolution and enhanced lock-upgrading semantics.
- **Semantic Renaming**: Internal cleanup of `StateManager` methods to align with `ManagedProvided` trait for better developer ergonomics.
- **Documentation**: New tutorial suite embedded in `src/tutorial.rs` for optimized `docs.rs` rendering.

## [0.1.0-alpha.1] - 2026-03-04

### Added

- `#[service]` macro - declarative long-running task registration with lifecycle management.
- `#[trigger]` macro - event-driven handlers with built-in host types:
  - `TT::Cron`, `TT::Signal` / `TT::Notify`, `TT::Queue` / `TT::BroadcastQueue`, `TT::Watch` / `TT::State`.
  - Custom hosts via `TriggerHost<T>` trait.
- `#[provider]` macro - compile-time dependency injection:
  - Struct providers, function providers, template providers (`Notify`, `Queue`).
  - `env = "VAR_NAME"` for environment variable binding.
- Resilience: exponential backoff with jitter, wave-based priority startup/shutdown, auto-restart on panic.
- Trigger interceptors (onion model): `TracingInterceptor`, `RetryInterceptor`, custom `TriggerInterceptor` trait.
- `StateManager` with tracked `RwLock`/`Mutex`, zero-lockdown snapshot reads, CoW with spurious wakeup prevention.
- Dependency graph cycle detection via `petgraph`.
- Structured `tracing` logging, optional file logging (`file-logging` feature).
- Elastic scaling for streaming triggers (`ScalingPolicy`).
- `MockContext` simulation support (feature-gated).
- `#![deny(unsafe_code)]` across the entire crate.

[unreleased]: https://github.com/loft-games/service-daemon-rs/compare/v0.1.0-alpha.3...HEAD
[0.1.0-alpha.3]: https://github.com/loft-games/service-daemon-rs/releases/tag/v0.1.0-alpha.3
[0.1.0-alpha.2]: https://github.com/loft-games/service-daemon-rs/releases/tag/v0.1.0-alpha.2
[0.1.0-alpha.1]: https://github.com/loft-games/service-daemon-rs/releases/tag/v0.1.0-alpha.1
