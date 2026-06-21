# Managed state & persistence pitfalls

## Calling `snapshot()` before initialization

`snapshot()` panics if the provider has not been initialized through the
daemon/provider resolution path. This is intentional (it flags caller misuse). In
services, inject `Arc<RwLock<T>>` / `Arc<Mutex<T>>` / `Arc<T>` instead of probing a
snapshot during setup.

## Expecting a no-op write to wake watchers

Acquiring a write guard and dropping it without mutating does not publish a change,
so `Watch` triggers will not fire. Only an actual mutation advances the value epoch.

## Holding a write guard across an await that triggers the watcher

Publication happens when the guard commits/drops. Holding the write guard for a long
time delays publication and can serialize other writers/readers. Scope the guard
tightly (mutate, then drop) before awaiting downstream work.

## Using normal memory for state that must survive a restart

In-memory fields are lost when a generation terminates. If a value must survive a
crash/restart, deposit it on the Shelf (`shelve`) and read it back (`unshelve`) in
the next generation. Don't assume struct fields persist across restarts.

## Assuming Shelf is shared across services

Shelf buckets are isolated by `ServiceId`. Two services cannot use the Shelf as a
shared channel — use managed state (`Arc<RwLock<T>>`) for cross-service sharing.

## Reaching for managed state when read-only would do

If state never mutates after init, inject a plain `Arc<T>` snapshot. Managed
`Arc<RwLock<T>>` only earns its cost when you actually mutate and/or watch.
