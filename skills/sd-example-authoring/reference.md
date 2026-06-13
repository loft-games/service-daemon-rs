# Example authoring reference

Detailed conventions for an `examples/<dir>` crate. Reference crates:
`examples/minimal/`, `examples/triggers/`, `examples/web-api/`.

## 1. Crate skeleton (`Cargo.toml`)

```toml
[package]
name = "example-<dir>"     # always the "example-" prefix + dir name
version = "0.1.0"
edition = "2024"
publish = false          # examples never publish to crates.io
description = "<one line: the framework mechanic this teaches>"

[dependencies]
service-daemon = { path = "../../service-daemon" }
# add features only if the example needs them, e.g.:
# service-daemon = { path = "../../service-daemon", features = ["simulation"] }
```

Then **add the directory to the root `Cargo.toml` `members` list** — that list is
explicit (not a glob), so an unlisted example is invisible to
`cargo build/test --workspace` and to CI.

## 2. Module layout

| File | Holds |
| :--- | :--- |
| `src/main.rs` | `#[tokio::main]` bootstrap: `ServiceDaemon::builder()...build()`, `run().await`, `wait().await?`. |
| `src/lib.rs` | Re-exports modules so `tests/` and the macros' link-time registries see them. |
| `src/providers.rs` | `#[provider]` definitions. |
| `src/services.rs` | `#[service]` definitions. |
| `src/trigger_handlers.rs` | `#[trigger]` handlers (triggers/web-api). |
| `tests/` | Integration tests exercising the example's topology. |

Every module opens with a module-level `//!` doc describing the **framework
mechanic** it teaches, not the pretend domain. See `examples/minimal/src/services.rs:1`.

## 3. Error-handling policy

Two deliberate idioms coexist:

- **Ergonomic path** — handler bodies return `anyhow::Result<()>` and use `?` for
brevity where the error meaning is uninteresting.
- **Explicit framework-error path** — resource acquisition returns a *structured*
  `ServiceError` so the supervisor's view stays visible instead of being flattened
  by `?`. Two equivalent forms, both real:

  ```rust
  // examples/minimal/src/services.rs — explicit match
  let l = match listener.get() {
      Ok(l) => l,
      Err(error) => return Err(ServiceError::runtime_io("clone TCP listener", error).into()),
  };

  // examples/web-api/src/services/daemon_services.rs — map_err + ?
  let l = listener
      .get()
      .map_err(|error| ServiceError::runtime_io("clone HTTP listener", error))?;
  ```

`ServiceError::runtime_io(operation: impl Into<String>, source: std::io::Error)`
lives at `service-daemon/src/models/error.rs:63`. Use the explicit form whenever a
bare `?` would hide what the framework observes (restart/backoff classification).

## 4. What to extract vs omit

- **Extract:** the framework topology — provider→service→trigger wiring, priority
  ordering, scheduling mode, the trigger host family being shown.
- **Drive with generic payloads** (`"Broadcast #{n}"`, counters), not domain data.
  See `examples/triggers/src/services.rs`.
- **Omit:** real product/business logic. An example reproduces the *shape* a real
  feature would take in the framework, never the feature itself.

## 5. Example responsibility layers

Keep `docs/development/release-validation.md` aligned with this classification:

| Layer | Examples | Responsibility |
| :--- | :--- | :--- |
| Tutorial path | `minimal`, `complete`, `triggers`, `simulation` | Teach the basic service, lifecycle, trigger, and test patterns. |
| Feature verification | `logging`, `diagnostics`, `scheduling`, `unix-domain-socket` | Keep non-default or focused framework features compiling and runnable. |
| Macro compile verification | `macro-tests` | Lock macro pass/fail behavior with compile-time tests. |
| Pressure and analysis | `stress`, `memory-analysis` | Measure scale and overhead; not production API contracts. |
| Adoption reference | `web-api`, `controller-bridge` | Show realistic integration shapes without turning every detail into a framework contract. |

When adding or reworking an example, update the release-validation map if its
layer or responsibility changes. Do not treat every example as a production
compatibility promise.

## 6. Contributor workflow

Before opening a PR (`docs/CONTRIBUTING.md`):

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
cargo expand -p example-complete   # inspect macro output when debugging codegen
```

Commits follow Conventional Commits.
