# Managed state & persistence reference

## 1. Managed state injection

Any `#[provider]` type can be injected in a managed form (the macro generates the
`ManagedProvided` capability):

- `Arc<RwLock<T>>` — multiple readers / single writer.
- `Arc<Mutex<T>>` — exclusive access.

These are tracked wrappers: acquiring a write guard and mutating, then committing or
dropping the guard, publishes a new snapshot of `T`.

```rust
async fn bump(metrics: Arc<RwLock<MetricsData>>) {
    let mut guard = metrics.write().await;
    guard.requests += 1;
    // publication happens when the guard commits / drops
}
```

## 2. Change publication and the watch path

- A write that actually mutates state publishes a new value: the tracked guard
  sends the replacement, advances the value epoch, and notifies waiters.
- A `Watch(T)` / `State(T)` trigger observes this through the generated
  `WatchableProvided::watch_dependency` baseline. When the value (or the provider
  binding) changes versus the baseline captured for the current generation, the
  watcher wakes and the trigger handler receives the new `Arc<T>` snapshot.
- Baselines are captured per service/trigger generation before the body becomes
  observable, so a change published between startup and the first poll is still
  seen (level-triggered, not edge-triggered).

## 3. Dirty tracking

Acquiring and releasing a write guard **without mutating** the value does not
publish a change. Only an actual mutation advances the epoch and wakes watchers.

## 4. `snapshot()` and read-only access

A managed provider's `snapshot()` convenience accessor returns the latest published
value, but it **panics if called before the provider has been initialized** through
the daemon/provider resolution path. That panic is intentional (caller misuse). In
normal services, obtain state through dependency injection (`Arc<RwLock<T>>` /
`Arc<Mutex<T>>` or a read-only `Arc<T>`), which resolves through the framework.

## 5. The Shelf (cross-generation persistence)

The Shelf is a per-service store whose contents survive generation termination and
are inherited by the next generation of the same service. Entries are **keyed** and
the functions are **async** — you must pass a `&str` key and `.await`:

| Function | Purpose |
| :--- | :--- |
| `shelve(key: &str, data: T).await` | Deposit `data` under `key` for the next generation. |
| `unshelve::<T>(key: &str).await -> Option<T>` | Retrieve **and remove** the value under `key`. |
| `shelve_clone::<T: Clone>(key: &str).await -> Option<T>` | **Non-destructive** read: a clone of the value under `key` (leaves it in place). |

Note `shelve_clone` does **not** deposit — it reads back a clone of something already
shelved (handy when a trigger host must re-read shelved state across iterations). To
deposit while keeping your local copy, call `shelve(key, value.clone()).await`.

Shelf buckets are isolated by `ServiceInstanceId`, so two selected services with
the same Rust function name do not share Shelf state. Related helpers:
`service_daemon::state` (observe lifecycle state) and
`service_daemon::current_service_instance_id`.

## 6. Choosing the mechanism

- Cross-service mutable state + notification → managed `Arc<RwLock<T>>` + `Watch`.
- Per-service durable checkpoint across restarts → Shelf.
- Read-only shared config → plain `Arc<T>` injection (no managed wrapper needed).
