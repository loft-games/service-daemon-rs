---
name: sd-daemon-bootstrap
description: "[user] Bootstrap and run a service-daemon-rs daemon from main(). Use when wiring up ServiceDaemon::builder(), selecting which services start via a tag-filtered Registry, ordering startup/shutdown by priority, or choosing between run() (non-blocking) and wait() (blocks until a signal)."
---

# Bootstrapping a service-daemon-rs daemon

Everything in this framework — services, triggers, providers — is discovered at
link time. `main()` does not register anything by hand; it uses
`ServiceDaemon::builder()` to construct a daemon instance handle, decides *which*
discovered definitions to select, starts auto-start services, and blocks until
shutdown.

The minimal, correct shape of `main()`:

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let daemon = service_daemon::ServiceDaemon::builder().build();
    daemon.run().await;     // spawns every service, returns immediately
    daemon.wait().await?;   // blocks until SIGINT / SIGTERM / Ctrl+C
    Ok(())
}
```

`run()` is **non-blocking**: it brings the daemon up wave by wave and returns.
`wait()` is what blocks the process. Calling only `run()` and dropping `daemon`
tears everything straight back down — a common first mistake.

## Choosing which services start

`build()` with no registry selects every statically-discovered service and trigger.
Selected services without `#[input]` auto-start one instance during startup.
Selected service templates that declare `#[input]` are available through
`ServiceHandle`, but do not auto-start. To run a subset, hand the builder a
tag-filtered `Registry`:

```rust
let registry = service_daemon::Registry::builder()
    .with_tag("web")      // additive: services tagged "web" ...
    .with_tag("worker")       // ... OR "worker"
    .exclude_tag("debug")     // minus anything tagged "debug"
    .build();

let daemon = service_daemon::ServiceDaemon::builder()
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
| `ServicePriority::EXTERNAL` | `0` | edge / inbound listeners (start last, stop first) |
| `ServicePriority::DEFAULT` | `50` | ordinary services |
| `ServicePriority::STORAGE` | `80` | databases, caches |
| `ServicePriority::SYSTEM` | `100` | core infra (start first, stop last) |

Set it with `#[service(priority = ServicePriority::STORAGE)]` or a bare number.

## Execution modes

For `scheduling = HighPriority`, first enable `high-priority` on the runtime
dependency. Standard uses the calling runtime; Isolated uses a private runtime
per generation; HighPriority permits automatic resource intervention and reload.
The mode is not a hard real-time guarantee. Its thresholds are internal, not
global public builder settings. See the reference's HighPriority section.

## File logging

When the `file-logging` feature is enabled, call `init_logging()` and then
`enable_file_logging(...)` before `run()`. `FileLogConfig::default()` writes
framework and application events to `logs/latest.log`. The first write after a
UTC day boundary closes and renames that file to `latest.YYYY-MM-DD.log`, then
compresses it in the background as `latest.YYYY-MM-DD.log.gz`. Shutdown leaves
the active file in place; restart uses its modification time to resume the same
period or rotate an older nonempty file before appending. A custom `file_prefix`
is the complete active filename; rotation inserts the period before its
extension. Existing rotations use numbered collision names and are not replaced.
Same-day merging uses `latest.YYYY-MM-DD.merged.log.gz`; maintenance also
recognizes the old `latest.log.YYYY-MM-DD` layout. Use
`FileLogCompression::None` when an external archive pipeline owns compression;
`RotationPolicy::Never` disables rotation and archive maintenance. The framework
does not delete old archives by count or age. Processes sharing a directory must
choose distinct directories or explicit prefixes.

## Companions

- `reference.md` — full builder method list, `run` vs `wait` vs `shutdown`,
  wave timeouts, `RestartPolicy`, and handoff to the simulation test harness.
- `pitfalls.md` — the traps: dropping the daemon, forgetting `wait()`, empty
  registry, tag typos, priority inversion.
- `examples/bootstrap.rs` — copy-paste `main()` variants.
