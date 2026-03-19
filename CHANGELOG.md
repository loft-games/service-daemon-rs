# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[unreleased]: https://github.com/loft-games/service-daemon-rs/compare/v0.1.0-alpha.1...HEAD
[0.1.0-alpha.1]: https://github.com/loft-games/service-daemon-rs/releases/tag/v0.1.0-alpha.1
