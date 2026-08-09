---
name: sd-simulation-testing
description: "[user] Test service-daemon-rs services deterministically with MockContext. Use when writing integration tests that run real services in an isolated sandbox, pre-fill or inject shelf state, override providers with fakes, drive a fixed-duration run, and assert on lifecycle status or shelved results."
---

# Deterministic testing with MockContext

The `simulation` feature ships a sandbox that runs **real** services under a real
`ServiceDaemon`, but with an isolated (empty) registry, a testing restart policy,
and injectable resources — so a test can pre-seed state, run for a bounded time,
mutate mid-flight, and read results back without standing up production infra.

Enable it in the test crate's `Cargo.toml`:

```toml
service-daemon = { path = "...", features = ["simulation"] }
# or for a published dep: service-daemon = { version = "...", features = ["simulation"] }
```

## The shape of a simulation test

```rust
use std::time::Duration;
use service_daemon::{MockContext, Registry};

#[tokio::test]
async fn service_reads_shelved_config() {
    // 1. Build the sandbox: pre-fill shelf, override providers, etc.
    let (builder, handle) = MockContext::builder()
        .with_shelf::<String>(svc_id, "config_key", "hello".into())
        .build(); // -> (ServiceDaemonBuilder, SimulationHandle)

    // 2. Select the real service(s) under test by tag, then build the daemon.
    let mut daemon = builder
        .with_registry(Registry::builder().with_tag("sim_shelf").build())
        .build();

    // 3. Run deterministically for a bounded time (simulation-only API).
    daemon.run_for_duration(Duration::from_millis(500)).await.ok();

    // 4. Assert via the lock-free read API on the handle.
    assert_eq!(
        handle.get_shelf::<String>(svc_id, "read_result"),
        Some("hello".to_string())
    );
}
```

`MockContext::builder().build()` returns a **`(ServiceDaemonBuilder, SimulationHandle)`**
pair. The builder is pre-isolated (no auto-discovery) — you opt services in by tag.
`run_for_duration(d)` is the deterministic driver and exists **only** under the
`simulation` feature.

## The two roles

- **`MockContextBuilder`** — *before* start: `with_shelf`, `with_status`,
  `with_provider_override`, `with_logging`.
- **`SimulationHandle`** (cloneable) — *during/after* the run: mutate with
  `set_shelf` / `set_status` / `trigger_reload` / `override_provider`, and read
  with the lock-free `get_shelf` / `get_status` / `has_shelf` / `shelf_keys`.

## Companions

- `reference.md` — every `MockContextBuilder` and `SimulationHandle` method,
  `ServiceEntryId` to `ServiceInstanceId` discovery, and the mid-flight mutation
  pattern.
- `pitfalls.md` — the traps (forgetting the feature, IDs before spawn, holding
  locks across await, no-op province of `run_for_duration`).
- `examples/simulation_test.rs` — a full pre-fill + mid-flight + assert test.
