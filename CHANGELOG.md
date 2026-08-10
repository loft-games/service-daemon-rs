# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Cross-platform Local IPC Providers**: Added `LocalIpcListen` and `LocalIpcConnect` provider templates that accept logical names and map them to Unix domain sockets or Windows named pipes while exposing a shared `AsyncRead`/`AsyncWrite` service shape.
- **Provider Attribute Normalization**: Added explicit `default = ...` and `template = ...` provider head forms while preserving existing bare defaults and built-in template syntax.
- **Release Security Contract**: Added maintainer documentation for deployment boundaries, logging safety, Unix socket deployment, TCP listener exposure, and example-production responsibilities.
- **Service Handles**: Added `ServiceHandle` and `service_handle!(...)` so provider code can resolve a daemon-local handle to a selected static service entry by service function path.
- **Service Registry Catalog**: Added a process-wide lazy service catalog with entry, tag, and wrapper-function indexes. Tag-filtered registries now build daemon-local projections from this catalog while preserving original `SERVICE_REGISTRY` order and `ServiceEntryId` values.
- **Windows Named Pipe Providers**: Added `NamedPipeListen` and `NamedPipeConnect` provider templates for Windows local IPC, including local-only pipe validation, first-instance ownership checks, reachability probes, and focused Windows MSVC validation coverage.

### Changed

- **Service Identity API**: Split static `ServiceEntryId` from UUIDv7-backed runtime `ServiceInstanceId`, removed the previous service identity type and entry-to-instance ID mapping, renamed service/trigger/logging context fields to `service_instance_id` / `source_service_instance_id`, and renamed trigger invocation identity to `TriggerInstanceId`.
- **Service Handle Resolution**: Provider-time service handle lookup is scoped to the current daemon projection. Linked services outside the projection and unlinked targets fail with provider initialization errors instead of falling back to a global runtime instance.
- **Provider Attribute Parser**: Shared named-tail parsing across service, trigger, and provider macros while keeping provider default/template heads as a macro-specific syntax boundary.
- **Proc-macro Diagnostics**: Removed the `proc-macro-error2` dependency and routed macro errors through the repository-owned diagnostics facade while preserving stable compile-error output.
- **Release Validation Gates**: Added `cargo deny --locked check` dependency-policy gating, documented the reviewed `cargo audit -D warnings` exception, and separated Windows local IPC provider checks into clearly named CI/manual gates.
- **UnixListen Helper API**: Renamed the generated `try_get().await?` listener-clone helper to synchronous `get()?`, matching the TCP `Listen` template and removing the alpha-era `try_get` surface.

### Fixed

- **Memory Analysis Example**: Updated the supervisor layout model to match the current service instance identity and runtime bookkeeping structures.
- **Private Service Visibility Diagnostics**: Stabilized the compile-time diagnostic for private service paths that are not visible to sibling modules.
- **Web API Swagger Routes**: Fixed the `examples/web-api` OpenAPI composition so Swagger documents preserve `/api/v1/...` paths without duplicate or missing prefixes.
- **Windows Named Pipe Providers**: Stabilized runtime ownership, busy-pipe retry behavior, listener replacement, invalid configuration diagnostics, and Windows-only compile gates.

## [0.1.0-alpha.5] - 2026-06-21

### Added

- **Runtime Facts Snapshots**: Added read-only daemon, readiness, service, and trigger runtime snapshots, including trigger self-pressure access through `TriggerContext::pressure()`.
- **Trigger Policy Overlays**: Added temporary trigger policy overlays through `TriggerContext::request_policy_overlay(...)` and `clear_policy_overlay(...)` for future-dispatch concurrency, timeout, and retry/backoff.
- **Trigger Context Constructor**: Added `TriggerContext::new(...)` for custom trigger engines and tests.
- **Public Diagnostics Snapshot**: Added read-only diagnostics snapshots with lifecycle facts, restart decisions, runtime-lane observations, bounded generation details, and Standard-lane interpretation hints.
- **Scheduling Advisory Controls**: Added `SchedulingAdvisoryProfile` so applications can suppress advisory diagnostics without changing lifecycle or body placement.
- **Provider Scope Ownership**: Added daemon-scoped provider ownership, binding epochs, and simulation override behavior so reload propagation can distinguish root, local, and override bindings.
- **Release Validation Map**: Added maintainer docs and skill rules for feature-to-test coverage, dependency baselines, linkme platform smoke coverage, and example responsibility layers.

### Changed

- **Diagnostics Topology Export**: Changed automatic shutdown topology export from direct stdout printing to a structured `tracing::info!` event carrying the Mermaid text in `topology_mermaid`.
- **Linkme Platform Monitoring**: Added positive release-mode linkme smoke and registry section/symbol inspection for Linux GNU, Linux musl, Windows MSVC, and macOS, while keeping Windows GNU as a best-effort XFAIL/workaround watchdog.
- **File Logging Contract**: Documented and tested file logging initialization failure as a warning plus console-only degradation instead of daemon startup failure.
- **Provider Macro Contract**: Reworked `#[provider]` item parsing around `syn::Item`, added explicit diagnostics for unsupported provider items and unsafe provider functions, and tightened function-provider `Result<T, ProviderError>` handling so same-named custom error types are rejected.
- **Generated Provider Helpers**: Kept helper return shapes tied to declared fallibility: direct helpers for infallible and framework-owned providers, `Result<_, ProviderInitError>` helpers for env, socket, dependency-injected, and `ProviderError` providers, with direct-helper panic diagnostics that include provider origin, definition location, helper callsite, module path, and the original provider-init error.
- **Provider Initialization Boundaries**: Clarified `ProviderError` versus `ProviderInitError`, preserved provider-init source classification in tracing/tests, and kept fatal, timeout, cancellation, panic, and dependency-provider failures distinct.
- **Scheduling Runtime**: Split supervision/control-plane work from service and trigger body execution lanes, added HighPriority worker-count selection from final registry entries, and kept advisory analysis read-only.
- **Trigger Supervision**: Improved trigger dispatch observability so retry exhaustion, infrastructure errors, panics, reload, and shutdown produce distinct lifecycle/restart signals.
- **Trigger Context Identity**: `TriggerContext` now carries the owning service generation. Manual struct literals should migrate to `TriggerContext::new(service_id, generation, instance_seq, message)` during the alpha API window.
- **Trigger Policy Overlay Contract**: Clarified temporary overlays as generation-scoped desired state for future dispatch scheduling. Concurrency is reconciled at runner scheduling boundaries; already captured timeout and retry policy are unchanged.
- **Logging Initialization**: Made logging initialization safe to call repeatedly and tightened log batch-size validation.

### Fixed

- **Runtime Facts Timeline**: Cleared stale `healthy_since` values when services leave `Healthy` and recorded recover/shutdown/termination boundaries consistently in service runtime facts.
- **Provider Reload Scope**: Made provider dependency reload generation-scoped so binding/value changes reload the affected generation without leaking across daemon boundaries.
- **Public API Boundaries**: Reduced accidental public surface area and tightened macro/runtime checks for invalid parameters and unsupported capacity values.
- **Panic Path Reduction**: Reworked several public-facing error paths to return structured errors instead of relying on panic behavior.
- **Shelf Isolation**: Isolated shelf status by `ServiceId` so duplicate Rust function names selected into different service entries do not share persisted generation state.

## [0.1.0-alpha.4] - 2026-05-07

### Added

- **Unix Domain Socket Providers**: Added `UnixListen` and `UnixConnect` provider templates for local IPC, sidecar coordination, and Unix-only service handoff patterns, including stale-socket recovery, non-socket path refusal, reachability probes, and retryable connection initialization.

### Changed

- **Provider Guides**: Documented Unix socket listener/client setup, reachability probes, stale-socket recovery, and error classification.
- **Scheduling Documentation**: Added `Standard`, `HighPriority`, and `Isolated` policy guidance to the tutorial, trigger guide, macro internals, and framework extension docs.

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

[unreleased]: https://github.com/loft-games/service-daemon-rs/compare/v0.1.0-alpha.5...HEAD
[0.1.0-alpha.5]: https://github.com/loft-games/service-daemon-rs/releases/tag/v0.1.0-alpha.5
[0.1.0-alpha.4]: https://github.com/loft-games/service-daemon-rs/releases/tag/v0.1.0-alpha.4
[0.1.0-alpha.3]: https://github.com/loft-games/service-daemon-rs/releases/tag/v0.1.0-alpha.3
[0.1.0-alpha.2]: https://github.com/loft-games/service-daemon-rs/releases/tag/v0.1.0-alpha.2
[0.1.0-alpha.1]: https://github.com/loft-games/service-daemon-rs/releases/tag/v0.1.0-alpha.1
