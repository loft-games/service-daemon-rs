---
name: sd-state-management
description: "[user] Use service-daemon-rs managed state and persistence. Use when sharing mutable state across services via Arc RwLock or Arc Mutex dependencies, when a state change must wake a Watch(T) trigger, or when state must survive a service restart (the Shelf)."
---

# Managed state & persistence in service-daemon-rs

The framework offers three distinct state mechanisms — pick by lifetime and intent:

| Need | Mechanism |
| :--- | :--- |
| Mutable state shared across services, with change notifications | managed state: inject `Arc<RwLock<T>>` / `Arc<Mutex<T>>` |
| React to a state change | a `Watch(T)` trigger on the provider |
| State that survives a generation restart/crash | the Shelf (`shelve` / `unshelve`) |

This SKILL.md is the entry point. Load the companions for depth:

- `reference.md` — how managed state is injected and published, the watch path,
  and the Shelf API.
- `pitfalls.md` — the subtle traps (snapshot-before-init panic, no-op writes,
  cross-service identity).
- `examples/` — copy-paste templates.

## Managed state

Declare the type with `#[provider]`, then inject the managed form:

```rust
#[service]
async fn writer(metrics: Arc<RwLock<MetricsData>>) -> anyhow::Result<()> {
    {
        let mut guard = metrics.write().await;
        guard.requests += 1;
    } // dropping the write guard publishes the change
    Ok(())
}
```

A managed write publishes a new snapshot when the write guard commits/drops. That
publication advances a value epoch and wakes anything watching the type.

## Reacting to changes

A `Watch(T)` trigger fires when the managed provider changes and receives the new
snapshot as `Arc<T>`:

```rust
#[trigger(Watch(MetricsData))]
async fn on_change(snapshot: Arc<MetricsData>) -> anyhow::Result<()> { Ok(()) }
```

## Surviving a restart (the Shelf)

State left in normal memory is lost when a generation terminates. Deposit it on the
per-service Shelf so the next generation of the same service inherits it:

```rust
service_daemon::shelve("checkpoint", value).await;            // deposit under a key
let prev: Option<Checkpoint> = service_daemon::unshelve("checkpoint").await; // retrieve+remove
```

See `reference.md` for `shelve_clone`, the watch-publication details, and the
`snapshot()` panic rule.
