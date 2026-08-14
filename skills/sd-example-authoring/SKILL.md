---
name: sd-example-authoring
description: "[dev] Author examples/* crates for the service-daemon-rs repo. Use when adding or reviewing a new example crate under examples/, to keep examples framework-topology focused and consistent with repo conventions."
---

# Authoring `examples/*` for service-daemon-rs

Conventions for adding a new example crate to this repo. Examples demonstrate
**framework topology**, not business logic.

## Crate conventions

- One crate per example dir: `name = "example-<dir>"`, `version = "0.1.0"`,
  `edition = "2024"`, `publish = false`, a one-line `description`, and dep
  `service-daemon = { path = "../../service-daemon" }` (see `examples/minimal/Cargo.toml`).
- Layout: `src/main.rs` + `lib.rs`, splitting into `providers.rs` / `services.rs`
  (triggers/web-api add `trigger_handlers.rs`). Use module-level `//!` docs that
  describe the **framework mechanic** the example teaches.
- Put integration tests under `tests/`.

## What an example should (and shouldn't) show

- **Demonstrate framework topology only.** Drive flows with generic payloads
  (e.g. `"Broadcast #{n}"`), not real domain semantics. See
  `examples/triggers/src/services.rs`.
- **No source business semantics.** Don't port real product logic into an example;
  extract just the framework wiring it would exercise.
- Classify every example in the release-validation layer before treating it as a
  contract: tutorial path, feature verification, macro compile verification,
  pressure/analysis, or adoption reference.

## Error handling in examples

Mixed and deliberate:

- Handlers may return `anyhow::Result<()>` and use `?` for ergonomics where the
  error meaning is uninteresting.
- Resource acquisition and IPC I/O paths should use explicit `match`/`if let`
  branches and, when the example has its own operation semantics, an example-local
  error enum under `src/models/`. This keeps each possible failure visible to the
  reader instead of flattening it behind one opaque `?`.

Use `ServiceError::runtime_io(...)` only when the failure is genuinely part of the
framework service runtime boundary being demonstrated. Do not wrap example-level
business or IPC operation errors as service-daemon framework errors just to avoid
declaring an example-local error type.

## Source of truth

- Reference examples: `examples/minimal/`, `examples/triggers/`,
  `examples/web-api/`, `examples/on-demand/`
- Contributor workflow: `docs/CONTRIBUTING.md`
  (`cargo test --workspace`, `cargo expand -p example-complete`,
  `cargo clippy --workspace -- -D warnings`, Conventional Commits)

## Companions

- `reference.md` — full crate skeleton, module layout, the two error-handling
  idioms (`ServiceError::runtime_io`), workspace-members requirement, and
  example responsibility layers.
- `pitfalls.md` — the authoring traps (unlisted member, swallowed errors,
  domain leakage, wrong metadata).
