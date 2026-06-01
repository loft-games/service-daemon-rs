# Simulation testing reference

All types are exported from the crate root under the `simulation` feature:
`MockContext`, `MockContextBuilder`, `SimulationHandle`. `run_for_duration` is a
method on `ServiceDaemon`, also `simulation`-gated.

## 1. `MockContext` / `MockContextBuilder` (pre-start setup)

`MockContext` is a zero-sized namespace. `MockContext::builder()` returns a
`MockContextBuilder`:

| Method | Effect |
| :--- | :--- |
| `with_shelf::<T: Any + Send + Sync>(service_id, key, data) -> Self` | Pre-fill a shelf entry (simulate previously persisted / crash-recovery data). |
| `with_status(service_id, status) -> Self` | Pre-set a service's lifecycle status (e.g. simulate a dependency's state). |
| `with_provider_override::<T: 'static + Send + Sync + Clone>(value) -> Self` | Install a fake provider value into the sandbox scope before start. |
| `with_logging(enable: bool) -> Self` | Include framework logging services. Default `true`; set `false` for lightweight tests. |
| `build() -> (ServiceDaemonBuilder, SimulationHandle)` | Produce the isolated builder + the handle. |

The returned `ServiceDaemonBuilder` is isolated: an empty registry (no
auto-discovery), a testing restart policy, and the pre-filled resources injected.
Call `.with_registry(Registry::builder().with_tag("...").build())` to opt the real
service(s) under test in by tag, then `.build()` the daemon.

## 2. `SimulationHandle` (runtime mutate + read)

Cloneable; holds `Arc`-backed resources shared with the running daemon.

**Mutators (during the run):**

| Method | Effect |
| :--- | :--- |
| `set_shelf::<T>(service_id, key, value)` | Inject a shelf entry mid-flight; visible on the service's next `unshelve`. |
| `set_status(service_id, status)` | Override lifecycle status and notify watchers. |
| `trigger_reload(&service_id)` | Fire the service's reload signal (wakes a `Watch` / reload waiter). |
| `override_provider::<T: 'static + Send + Sync + Clone>(value)` | Swap a provider binding; watching generations reload via the provider watch path. |
| `service_ids() -> Vec<ServiceId>` | List runtime IDs **after** the runner has spawned services. |

**Lock-free readers (safe across `.await`):**

| Method | Returns |
| :--- | :--- |
| `get_shelf::<T: Clone>(service_id, key) -> Option<T>` | Owned clone of a shelf value. The recommended way to assert. |
| `get_status(service_id) -> Option<ServiceStatus>` | Owned current status. |
| `has_shelf(service_id, key) -> bool` | Whether a key exists. |
| `shelf_keys(service_id) -> Vec<String>` | All shelf keys for the service. |

## 3. Driving the run

`ServiceDaemon::run_for_duration(self, Duration) -> ServiceResult<()>` runs the
daemon, then auto-shuts-down after the duration. Deterministic and `simulation`
-only. For a **mid-flight** mutation, spawn the run and mutate via the handle while
it is in flight:

```rust
let h = handle.clone();
let task = tokio::spawn(async move { daemon.run_for_duration(Duration::from_secs(3)).await.ok(); });
tokio::time::sleep(Duration::from_millis(200)).await; // let services spawn
h.set_shelf::<String>(svc_id, "dynamic_key", "injected".into());
task.await.ok();
assert_eq!(h.get_shelf::<String>(svc_id, "dynamic_result"), Some(/* ... */));
```

## 4. Obtaining a `ServiceId`

`with_shelf` / `with_status` need a `ServiceId` *before* the daemon starts.
`ServiceId` is the service's strong identity assigned at registry build. After the
runner has spawned services you can enumerate IDs with `handle.service_ids()`; for
pre-fill you derive the id for the service under test from its registry identity.
Keep the service-under-test simple and single, so its id is unambiguous.

## 5. `ServiceStatus` values

`Initializing`, `Restoring`, `Recovering(String)`, `Healthy`, `NeedReload`,
`ShuttingDown`, `Terminated`. Assert against these via `get_status`.
