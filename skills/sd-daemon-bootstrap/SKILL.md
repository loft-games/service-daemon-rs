---
name: sd-daemon-bootstrap
description: "[user] Bootstrap and run a service-daemon-rs daemon from main(). Use when wiring up ServiceDaemon::builder(), selecting which services start via a tag-filtered Registry, ordering startup/shutdown by priority, or choosing between run() (non-blocking) and wait() (blocks until a signal)."
---

# Bootstrapping a service-daemon-rs daemon

Everything in this framework — services, triggers, providers — is discovered at
link time. `main()` does not register anything by hand; it constructs a
`ServiceDaemon`, decides *which* of the discovered services to run, starts them,
and blocks until shutdown.

The minimal, correct shape of `main()`:

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut daemon = service_daemon::ServiceDaemon::builder().build();
    daemon.run().await;     // spawns every service, returns immediately
    daemon.wait().await?;   // blocks until SIGINT / SIGTERM / Ctrl+C
    Ok(())
}
```

`run()` is **non-blocking**: it brings the daemon up wave by wave and returns.
`wait()` is what blocks the process. Calling only `run()` and dropping `daemon`
tears everything straight back down — a common first mistake.

## Choosing which services start

`build()` with no registry runs **every** statically-discovered service. To run a
subset, hand the builder a tag-filtered `Registry`:

```rust
let registry = service_daemon::Registry::builder()
    .with_tag("web")      // additive: services tagged "web" ...
    .with_tag("worker")       // ... OR "worker"
    .exclude_tag("debug")     // minus anything tagged "debug"
    .build();

let mut daemon = service_daemon::ServiceDaemon::builder()
    .with_registry(registry)
    .build();
```

Tags come from `#[service(tags = ["web"])]` / `#[trigger(..., tags = ["web"])]`.
`with_tag` is **OR** (union), `exclude_tag` removes. This is how one binary serves
multiple deployment shapes without `cfg` gymnastics.

## Startup / shutdown ordering

Services start in **descending priority** (high first) and stop in **ascending
priority** (low first), so infrastructure comes up before its dependents and
shuts down after them. Priority is a `u8`, default `50`. Named constants:

| Constant | Value | Typical use |
| :--- | :--- | :--- |
| `Priority::EXTERNAL` | `0` | edge / inbound listeners (start last, stop first) |
| `Priority::DEFAULT` | `50` | ordinary services |
| `Priority::STORAGE` | `80` | databases, caches |
| `Priority::SYSTEM` | `100` | core infra (start first, stop last) |

Set it with `#[service(priority = Priority::STORAGE)]` or a bare number.

## Companions

- `reference.md` — full builder method list, `run` vs `wait` vs `shutdown`,
  wave timeouts, `RestartPolicy`, and the simulation-only `run_for_duration`.
- `pitfalls.md` — the traps: dropping the daemon, forgetting `wait()`, empty
  registry, tag typos, priority inversion.
- `examples/bootstrap.rs` — copy-paste `main()` variants.
