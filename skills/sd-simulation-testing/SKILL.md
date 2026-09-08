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
    // 1. Select the real service under test and derive its pre-start ID.
    let registry = Registry::builder().with_tag("sim_shelf").build();
    let svc_id = registry
        .services()
        .iter()
        .find(|service| service.name() == "shelf_reader_service")
        .and_then(|service| service.instance_ids().first().copied())
        .expect("service should be materialized in registry");

    // 2. Build the sandbox: pre-fill shelf, override providers, etc.
    let simulation = MockContext::builder()
        .with_shelf::<String>(svc_id, "config_key", "hello".into())
        .with_registry(registry)
        .build();

    // 3. Run deterministically for a bounded time.
    simulation.run_for_duration(Duration::from_millis(500)).await.ok();

    // 4. Assert via the lock-free read API on the runtime handle.
    let service = simulation
        .service_instances()
        .into_iter()
        .find(|instance| instance.name() == "shelf_reader_service")
        .expect("service should be materialized in simulation daemon");
    assert_eq!(
        simulation.get_shelf::<String>(&service, "read_result"),
        Some("hello".to_string())
    );
}
```

`MockContext::builder().with_registry(...).build()` returns a cloneable
**`SimulationHandle`** wrapping an isolated daemon. The builder starts with an
empty registry; opt services in by tag before `build()`. `run_for_duration(d)` is
the deterministic driver on `SimulationHandle` and exists **only** under the
`simulation` feature.

## HighPriority verification boundary

`simulation` does not enable `high-priority`; enable both when a test declares
that execution mode. MockContext validates service behavior and lifecycle with
test resources, not production tail latency or the benefit of adding shards.
Do not treat bounded-duration simulation as virtual-time proof of a real
intervention deadline. Maintainer validation separates deterministic controller
tests, real supervisor/reload integration, and a separately run load experiment;
see `docs/development/release-validation.md` in the framework repository.

## The two roles

- **`MockContextBuilder`** — *before* start: `with_shelf`, `with_status`,
  `with_provider_override`, `with_logging`.
- **`SimulationHandle`** (cloneable) — *during/after* the run: mutate with
  `set_shelf` / `set_status` / `trigger_reload` / `override_provider`, and read
  with the lock-free `get_shelf` / `get_status` / `has_shelf` / `shelf_keys`.
  Runtime mutation/read APIs take `&ServiceInstanceHandle`.

## Companions

- `reference.md` — every `MockContextBuilder` and `SimulationHandle` method,
  pre-start `ServiceInstanceId` discovery, runtime `ServiceInstanceHandle`
  discovery, and the mid-flight mutation pattern.
- `pitfalls.md` — the traps (forgetting the feature, IDs before spawn, holding
  locks across await, no-op province of `run_for_duration`).
- `examples/simulation_test.rs` — a full pre-fill + mid-flight + assert test.
