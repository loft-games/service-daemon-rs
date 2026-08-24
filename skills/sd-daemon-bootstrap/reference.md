# Daemon bootstrap reference

## 1. `ServiceDaemon::builder()` → `ServiceDaemonBuilder`

Every method is chainable and optional. `build()` is infallible and returns a
`DaemonInstanceHandle`.

| Method | Purpose |
| :--- | :--- |
| `with_registry(Registry)` | Restrict which discovered services run. Default = all static services. |
| `with_restart_policy(RestartPolicy)` | Supervision: restart limits, backoff, provider-init timeout, wave timeouts. |
| `with_scheduling_advisory_profile(SchedulingAdvisoryProfile)` | Tune the scheduling advisory plane. |
| `with_isolated_startup_concurrency_limit(NonZeroUsize)` | Cap how many `Isolated`-scheduled services start at once. |
| `with_cancel_token(CancellationToken)` | Supply an external token so something other than a signal can drive shutdown. |
| `with_trigger_config<C: 'static + Clone + Send + Sync>(C)` | Inject a typed config object visible to trigger hosts. |
| `with_infra_tags(&[&'static str])` | Mark certain tags as infrastructure for ordering/advisory purposes. |
| `build()` | Produce and register the daemon instance handle. Never fails. |

```rust
let daemon = ServiceDaemon::builder()
    .with_registry(registry)
    .with_restart_policy(policy)
    .build();
```

## 2. Selecting services — `Registry::builder()` → `RegistryBuilder`

| Method | Semantics |
| :--- | :--- |
| `with_tag(&'static str)` | Include services carrying this tag (**union / OR** across calls). |
| `with_tags(impl IntoIterator<Item = impl Into<&'static str>>)` | Bulk `with_tag`. |
| `exclude_tag(&'static str)` | Remove services carrying this tag (applied after inclusion). |
| `build()` | Produce the `Registry`. |

A registry with no `with_tag` selects nothing extra by tag — be explicit about
your selection model. `build()` with no registry at all (skip `with_registry`)
selects every discovered service. Selected services without `#[input]` auto-start
one instance; selected service templates with `#[input]` are definition handles
only until code calls `ServiceHandle::create(input)` or `start(input)`.

## 3. Lifecycle methods on the daemon handle

| Method | Blocking? | Behavior |
| :--- | :--- | :--- |
| `run(&self)` | No | Brings the daemon up wave by wave, then returns. |
| `wait(&self) -> ServiceResult<()>` | Yes | Blocks until SIGINT / SIGTERM / Ctrl+C or the cancel token fires, then performs graceful shutdown. |
| `shutdown(&self)` | No | Signals shutdown from elsewhere (another task, a handler). |

The usual pairing is `run().await` then `wait().await?`. `run()` alone does
not keep the process alive.

## 4. Wave ordering and priority

- Startup proceeds in **descending** priority (high values first); shutdown in
  **ascending** priority (low values first).
- Priority is a `u8`, default `50`. Constants (`service_daemon::ServicePriority`):
  `EXTERNAL = 0`, `DEFAULT = 50`, `STORAGE = 80`, `SYSTEM = 100`.
- Each wave waits for its members to become healthy before the next wave starts,
  bounded by `RestartPolicy::wave_spawn_timeout` (and `wave_stop_timeout` on the
  way down).

## 5. `RestartPolicy` (`RestartPolicy::builder()` → `RestartPolicyBuilder`)

Relevant knobs for bootstrap:

- `provider_init_timeout(Duration)` — bounds how long a `Retryable` provider
  init may keep retrying before the dependent service is failed.
- `wave_spawn_timeout` / `wave_stop_timeout` — per-wave health/stop deadlines
  (public fields on `RestartPolicy`).

```rust
let policy = RestartPolicy::builder()
    .provider_init_timeout(Duration::from_secs(30))
    .build();
```

## 6. HighPriority runtime control

`#[service(scheduling = HighPriority)]` and `#[trigger(..., scheduling = HighPriority)]`
are the source-level declarations for the HighPriority mode. The daemon manages
that mode internally at runtime.

The daemon uses conservative HighPriority runtime control:

- initial capacity comes from the final selected HighPriority service/trigger
  entries;
- total HighPriority worker threads are capped by available CPU parallelism
  by default;
- sustained shard probe pressure can create additional HighPriority shards;
- cooperative rollover can ask an existing HighPriority generation to reload so
  the next generation receives a better shard placement.

`SchedulingAdvisoryProfile` is separate: disabling advisory logs does not disable
HighPriority scale-out or rollover.

## 7. Choosing production vs simulation execution

- Production binary → `run().await; wait().await?;`
- Deterministic test under the `simulation` feature → `MockContext` /
  `SimulationHandle::run_for_duration(d)`.
  See the `sd-simulation-testing` skill for the full test harness.
