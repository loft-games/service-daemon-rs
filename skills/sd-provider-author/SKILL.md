---
name: sd-provider-author
description: "[user] Author service-daemon-rs #[provider] dependency providers. Use when writing or reviewing a #[provider] (value, env, template, struct, or async-fn form) for the service-daemon Rust framework, or when deciding fallible Result<T, ProviderError> error semantics (Fatal vs Retryable) and lazy vs eager initialization."
---

# Authoring `#[provider]` for service-daemon-rs

`#[provider]` declares a type-injectable dependency. Injection sites resolve it as
`Arc<T>` (or `Arc<RwLock<T>>` / `Arc<Mutex<T>>` for managed state). The macro
auto-generates all the DI plumbing — you write the value or the init logic.

This SKILL.md is the entry point. Load the companion files for depth:

- `reference.md` — every provider form and its exact syntax, the `ProviderError`
  model, the init/retry engine, lazy vs eager, and the DI traits.
- `pitfalls.md` — the mistakes that compile but misbehave.
- `examples/` — copy-paste templates for each form.

## Pick the form

| You need… | Form | Example |
| :--- | :--- | :--- |
| A constant/default value | value | `#[provider(8080)] struct Port(pub i32);` |
| A value from env (with/without default) | env | `#[provider(8080, env = "PORT")] struct Port(pub i32);` |
| A queue / signal / socket source | template | `#[provider(Queue(Job))] struct Jobs;` |
| A struct composed from other providers | struct | `#[provider] struct Cfg { db: Arc<DbUrl> }` |
| Async/fallible construction (clients, pools) | async fn | `#[provider] async fn pool() -> Result<Pool, ProviderError>` |

Full syntax for each (templates `Notify`/`Event`/`Queue`/`BQueue`/`Listen`/
`UnixListen`/`UnixConnect`, the `env`/`capacity`/`eager` attributes) is in
`reference.md`.

## The two decisions that matter most

**1. If init can fail, return `Result<T, ProviderError>` and classify correctly:**

- `ProviderError::Fatal(msg)` — cannot recover (bad config, missing local
  resource). The daemon **fails fast** and shuts down. No retry.
- `ProviderError::Retryable(msg)` — transient (upstream not ready). The daemon
  **retries with backoff** until `RestartPolicy::provider_init_timeout`, then the
  run is treated as a terminal timeout failure.

Return the **plain `T`** (e.g. `Ok(pool)`), never `Arc<T>` — the framework wraps it.

**2. Lazy (default) vs eager:** providers initialize lazily on first resolution.
Add `eager = true` only when a failure must abort startup before dependents run
(e.g. DB migrations). See `reference.md` for reachability rules.

## Hard rules

- Don't reimplement retry/timeout in the provider body — return `ProviderError`
  and let the framework own retry, fatal shutdown, and cancellation.
- Don't return `Arc<T>`; return `T`.
- Don't read a managed provider's snapshot before it is initialized (it panics by
  design — see `pitfalls.md`).
