---
name: sd-trigger-author
description: "[user] Author service-daemon-rs #[trigger] handlers. Use when writing or reviewing a #[trigger] for the service-daemon Rust framework: choosing the host (Cron/Queue/Event/Watch), wiring it to a provider target, and declaring the payload (by value) vs dependencies (Arc<T>)."
---

# Authoring `#[trigger]` for service-daemon-rs

A `#[trigger]` is a specialized service that runs a handler when an event source
fires. The source is a `#[provider]` type; the macro wires the handler to the
matching host and supervises it like any service.

This SKILL.md is the entry point. Load the companions for depth:

- `reference.md` — the host families, exact attribute syntax, payload vs
  dependency rules, and how each host pairs with a provider.
- `pitfalls.md` — the mistakes that compile but misbehave.
- `examples/` — copy-paste handler templates for each host.

## Syntax

```
#[trigger(Host(Target))]
#[trigger(Host(Target), priority = N)]
```

`Host` is one of the host families below; `Target` is the `#[provider]` type that
supplies the event source. Handlers are `async` and return `anyhow::Result<()>`.

| Host family | Fires when… | Target provider | Handler payload |
| :--- | :--- | :--- | :--- |
| `Cron(T)` | a cron schedule ticks | provider yielding the cron `String` | none |
| `Queue(T)` / `BQueue(T)` / `BroadcastQueue(T)` | an item is published | `#[provider(Queue(T))]` | the item, by value |
| `Event(T)` / `Notify(T)` / `Signal(T)` / `Custom(T)` | a signal is raised | `#[provider(Notify)]` | none |
| `Watch(T)` / `State(T)` | the target provider's state changes | any provider | `Arc<T>` snapshot |

## The rules that matter most

1. **Payload is by value; dependencies are `Arc<T>`.** A queue handler takes the
   item by value (`item: String`); a watch handler takes `Arc<T>`. Any additional
   `#[provider]` dependency is injected as `Arc<T>`.
2. **The `Target` must be a provider compatible with the host** (e.g. a `Queue`
   host needs a `#[provider(Queue(T))]` target). See the `sd-provider-author`
   skill for declaring sources.
3. **Triggers are supervised like services** — keep handlers async, return
   `anyhow::Result<()>`; a handler `Err` is retried by the trigger runner, and
   `priority`/`scheduling`/`tags` work the same as `#[service]`.
