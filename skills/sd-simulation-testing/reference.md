# Simulation testing reference

All types are exported from the crate root under the `simulation` feature:
`MockContext`, `MockContextBuilder`, and `SimulationHandle`. Simulation runs are
driven through `SimulationHandle`; production `ServiceDaemon` does not expose
these helpers.

## 1. `MockContext` / `MockContextBuilder` (pre-start setup)

`MockContext` is a zero-sized namespace. `MockContext::builder()` returns a
`MockContextBuilder`:

| Method | Effect |
| :--- | :--- |
| `with_shelf::<T: Any + Send + Sync>(service_instance_id, key, data) -> Self` | Pre-fill a shelf entry (simulate previously persisted / crash-recovery data). |
| `with_status(service_instance_id, status) -> Self` | Pre-set a service's lifecycle status (e.g. simulate a dependency's state). |
| `with_provider_override::<T: 'static + Send + Sync + Clone>(value) -> Self` | Install a fake provider value into the sandbox scope before start. |
| `with_logging(enable: bool) -> Self` | Include framework logging services. Default `true`; set `false` for lightweight tests. |
| `with_registry(Registry) -> Self` | Select the real service(s) to run inside the isolated sandbox. |
| `build() -> SimulationHandle` | Produce the isolated daemon handle. |

The returned `SimulationHandle` wraps an isolated daemon: empty registry by
default, a testing restart policy, and pre-filled resources injected. Call
`.with_registry(Registry::builder().with_tag("...").build())` before `.build()` to
opt the real service(s) under test in by tag.

## 2. `SimulationHandle` (runtime mutate + read)

Cloneable; holds `Arc`-backed resources shared with the running daemon.

**Mutators (during the run):**

| Method | Effect |
| :--- | :--- |
| `run().await` / `wait().await` / `shutdown()` | Start, wait for, or stop the sandbox daemon. |
| `run_for_duration(duration).await` | Run the sandbox daemon for a bounded duration and shut it down. |
| `service_instances() -> Vec<ServiceInstanceHandle>` | List runtime instance handles after the runner has spawned services. |
| `service_instances_for(&ServiceHandle) -> Vec<ServiceInstanceHandle>` | List runtime instance handles for one selected service definition. |
| `set_shelf::<T>(&ServiceInstanceHandle, key, value)` | Inject a shelf entry mid-flight; visible on the service's next `unshelve`. |
| `set_status(&ServiceInstanceHandle, status)` | Override lifecycle status and notify watchers. |
| `trigger_reload(&ServiceInstanceHandle)` | Fire the service's reload signal (wakes a `Watch` / reload waiter). |
| `override_provider::<T: 'static + Send + Sync + Clone>(value)` | Swap a provider binding; watching generations reload via the provider watch path. |

**Lock-free readers (safe across `.await`):**

| Method | Returns |
| :--- | :--- |
| `get_shelf::<T: Clone>(&ServiceInstanceHandle, key) -> Option<T>` | Owned clone of a shelf value. The recommended way to assert. |
| `get_status(&ServiceInstanceHandle) -> Option<ServiceStatus>` | Owned current status. |
| `has_shelf(&ServiceInstanceHandle, key) -> bool` | Whether a key exists. |
| `shelf_keys(&ServiceInstanceHandle) -> Vec<String>` | All shelf keys for the service. |

## 3. Driving the run

`SimulationHandle::run_for_duration(Duration) -> ServiceResult<()>` runs the
sandbox daemon, then auto-shuts-down after the duration. Deterministic and
`simulation`-only. For a **mid-flight** mutation, clone the handle, spawn the run,
then discover `ServiceInstanceHandle`s after the runner has started:

```rust
let runner = simulation.clone();
let task = tokio::spawn(async move { runner.run_for_duration(Duration::from_secs(3)).await.ok(); });
tokio::time::sleep(Duration::from_millis(200)).await; // let services spawn
let instance = simulation
    .service_instances()
    .into_iter()
    .find(|instance| instance.name() == "service_under_test")
    .expect("service should be materialized");
simulation.set_shelf::<String>(&instance, "dynamic_key", "injected".into());
task.await.ok();
assert_eq!(simulation.get_shelf::<String>(&instance, "dynamic_result"), Some(/* ... */));
```

## 4. Obtaining service IDs and handles

`with_shelf` / `with_status` need a `ServiceInstanceId` *before* the daemon starts.
Build the same `Registry` that the simulation will use, then read the materialized
ID from the selected `ServiceDescription`:

```rust
let registry = Registry::builder().with_tag("sim_shelf").build();
let svc_id = registry
    .services()
    .iter()
    .find(|service| service.name() == "shelf_reader_service")
    .and_then(|service| service.instance_ids().first().copied())
    .expect("service should be materialized in registry");
let simulation = MockContext::builder()
    .with_shelf::<String>(svc_id, "config_key", "hello".into())
    .with_registry(registry)
    .build();
```

After the runner has spawned services, use `SimulationHandle::service_instances()`
or `service_instances_for(&ServiceHandle)` and pass `&ServiceInstanceHandle` to
runtime mutation/read APIs. Keep the service-under-test simple and single, so the
selected instance is unambiguous.

## 5. `ServiceStatus` values

`Initializing`, `Restoring`, `Recovering(String)`, `Healthy`, `NeedReload`,
`ShuttingDown`, `Terminated`. Assert against these via `get_status`.
