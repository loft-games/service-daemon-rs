---
name: sd-provider-author
description: "[user] Author service-daemon-rs dependency providers. Use when writing or reviewing #[provider] (value, env, template, struct, or async-fn form), #[provider_contract], #[provider_impl], fallible Result provider error semantics (Fatal vs Retryable vs Unavailable), and lazy vs eager initialization."
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
| Cross-crate shared contract | contract + impl | `#[provider_contract] struct SharedSettings;` + `#[provider_impl] async fn settings() -> SharedSettings` |

Full syntax for each (templates `Notify`/`Event`/`Queue`/`BQueue`/`Listen`/
`UnixListen`/`UnixConnect`/`NamedPipeListen`/`NamedPipeConnect`/
`LocalIpcListen`/`LocalIpcConnect`, the `env`/`capacity`/`eager` attributes) is
in `reference.md`.

## The two decisions that matter most

**1. If init can fail, return `Result<T, ProviderError>` and classify correctly:**

- `ProviderError::Fatal(msg)` — cannot recover (bad config, missing local
  resource). The daemon **fails fast** and shuts down. No retry.
- `ProviderError::Retryable(msg)` — transient (upstream not ready). The daemon
  **retries with backoff** until `RestartPolicy::provider_init_timeout`, then the
  run is treated as a terminal timeout failure.
- `ProviderError::Unavailable(msg)` — only for `#[provider_impl]` candidates that
  do not apply in the current deployment. The contract tries the next candidate.
  In ordinary `#[provider]`, there is no fallback, so `Unavailable` is fatal.

Return the **plain `T`** (e.g. `Ok(pool)`), never `Arc<T>` — the framework wraps it.

**2. Lazy (default) vs eager:** providers initialize lazily on first resolution.
Add `eager = true` only when a failure must abort startup before dependents run
(e.g. DB migrations). See `reference.md` for reachability rules.

## Hard rules

- Don't reimplement retry/timeout in the provider body — return `ProviderError`
  and let the framework own retry, fatal shutdown, and cancellation.
- Don't return `Arc<T>`; return `T`.
- For cross-crate providers, put `#[provider_contract]` on the shared output type
  and `#[provider_impl(priority = N)]` on app-local functions. Do not use
  ordinary `#[provider]` to implement DI traits for a foreign type.
- Candidate dependencies initialize only when fallback reaches that candidate.
  If any candidate uses `service_handle!`, the contract is daemon-local; do not
  assume it shares the root cache. Use `#[provider_contract(eager = true)]` only
  when a reachable contract must resolve during startup.
- Don't read a managed provider's snapshot before it is initialized (it panics by
  design — see `pitfalls.md`).
