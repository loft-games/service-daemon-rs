# `#[provider]` pitfalls

Mistakes that compile (or look right) but misbehave at runtime.

## Returning `Arc<T>` from a provider

Providers return the plain value `T`. The framework wraps it in `Arc<T>`. Returning
`Arc<T>` yourself is wrong — return `T` (or `Ok(T)` for fallible providers).

## Wrong `ProviderError` classification

- Returning `Retryable` for permanently-broken input (bad config, missing required
  file) does not help: the framework retries until `provider_init_timeout` and then
  fails anyway, just slower. Use `Fatal` for unrecoverable conditions.
- Returning `Fatal` for a merely-not-ready-yet upstream kills the daemon when a
  bounded retry would have succeeded. Use `Retryable` for transient conditions.

## Forgetting the retry window

`RestartPolicy::provider_init_timeout` defaults to `wave_spawn_timeout`. If a
`Retryable` provider talks to an upstream that takes longer than that to come up,
init will time out. Raise `provider_init_timeout` explicitly on the daemon's
restart policy.

## Reimplementing retry inside the provider body

Don't loop/sleep/retry inside the provider. Return `ProviderError::Retryable` and
let the framework own backoff, the timeout deadline, and cancellation. A hand-rolled
loop ignores the cancellation token and the startup deadline.

## Reading managed state before init

A managed provider's snapshot accessor panics if called before that provider has
been initialized through the daemon/provider resolution path. This panic is
intentional — it signals caller misuse, not a recoverable error. In normal services
use dependency injection (`Arc<T>` / `Arc<RwLock<T>>`) so resolution happens through
the framework.

## Named attributes inside template parentheses

`#[provider(Listen("addr", env = "VAR"))]` is a compile error. Named attributes go
outside: `#[provider(Listen("addr"), env = "VAR")]`.

## `capacity` on the wrong form

`capacity = N` is only valid on `Queue`/`BQueue` and must be `> 0`. Using it on a
value provider is a compile error.

## Env-only provider with a missing variable

`#[provider(env = "API_KEY")]` (no default) fails initialization if the env var is
absent. Provide a default (`#[provider("fallback", env = "API_KEY")]`) when the
value is optional.
