# Daemon bootstrap reference

## 1. `ServiceDaemon::builder()` → `ServiceDaemonBuilder`

Every method is chainable and optional. `build()` is infallible.

| Method | Purpose |
| :--- | :--- |
| `with_registry(Registry)` | Restrict which discovered services run. Default = all static services. |
| `with_restart_policy(RestartPolicy)` | Supervision: restart limits, backoff, provider-init timeout, wave timeouts. |
| `with_scheduling_advisory_profile(SchedulingAdvisoryProfile)` | Tune the scheduling advisory plane. |
| `with_isolated_startup_concurrency_limit(NonZeroUsize)` | Cap how many `Isolated`-scheduled services start at once. |
| `with_cancel_token(CancellationToken)` | Supply an external token so something other than a signal can drive shutdown. |
| `with_trigger_config<C: 'static + Clone + Send + Sync>(C)` | Inject a typed config object visible to trigger hosts. |
| `with_infra_tags(&[&'static str])` | Mark certain tags as infrastructure for ordering/advisory purposes. |
| `build()` | Produce the `ServiceDaemon`. Never fails. |

```rust
let mut daemon = ServiceDaemon::builder()
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
runs every discovered service.

## 3. Lifecycle methods on `ServiceDaemon`

| Method | Blocking? | Behavior |
| :--- | :--- | :--- |
| `run(&mut self) -> &mut Self` | No | Brings the daemon up wave by wave, then returns. |
| `wait(&mut self) -> ServiceResult<()>` | Yes | Blocks until SIGINT / SIGTERM / Ctrl+C or the cancel token fires, then performs graceful shutdown. |
| `shutdown(&self)` | No | Signals shutdown from elsewhere (another task, a handler). |
| `run_for_duration(self, Duration) -> ServiceResult<()>` | Yes | **`#[cfg(feature = "simulation")]` only** — run, then auto-shutdown after the duration. For deterministic tests. |

The usual pairing is `run().await` then `wait().await?`. `run()` alone does
not keep the process alive.

## 4. Wave ordering and priority

- Startup proceeds in **descending** priority (high values first); shutdown in
  **ascending** priority (low values first).
- Priority is a `u8`, default `50`. Constants (`service_daemon::Priority`):
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

## 6. Choosing run vs run_for_duration

- Production binary → `run().await; wait().await?;`
- Deterministic test under the `simulation` feature → `run_for_duration(self, d)`.
  See the `sd-simulation-testing` skill for the full test harness.
