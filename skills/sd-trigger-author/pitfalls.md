# `#[trigger]` pitfalls

## Payload type mismatch with the queue

A `Queue(T)` handler's payload parameter must match the provider's payload type.
`#[provider(Queue(String))]` requires `async fn(item: String)`, not `Arc<String>`
and not a different type.

## Using `Arc<T>` for a queue payload

The queue payload is passed **by value** (auto-cloned per subscriber). Declaring it
as `Arc<T>` is wrong — that is the shape for `Watch(T)` snapshots and for injected
dependencies, not for queue items.

## Wrong host for the target provider

The host family must match the provider template: `Queue` host ↔
`#[provider(Queue(T))]`, `Event`/`Notify`/`Signal` host ↔ `#[provider(Notify)]`,
`Cron` host ↔ a provider yielding the cron `String`. Mismatches fail to resolve.

## Expecting a payload on Cron/Event triggers

`Cron` and the signal family (`Event`/`Notify`/`Signal`/`Custom`) carry no payload.
The handler takes only injected `Arc<T>` dependencies (if any), no value parameter.

## Blocking the handler

Triggers run on the event-loop host; keep handlers async and non-blocking. For
genuinely blocking work, declare `scheduling = Isolated` so it runs on a dedicated
thread instead of stalling dispatch.

## Treating handler `Err` as fatal

A handler `Err(..)` is retried by the runner, not an immediate hard stop. If a
condition is genuinely unrecoverable, handle it explicitly rather than relying on
repeated retries.

## Reaching for interceptors as a public API

`TriggerInterceptor` is internal — there is no public registration hook. To add a
new event source, implement `TriggerHost<T>` (a maintainer/extension task).
