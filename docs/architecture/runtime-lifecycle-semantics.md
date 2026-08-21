# Runtime Lifecycle Semantics

> Status: architecture note.
>
> Scope: internal runtime lifecycle semantics. This document describes how reload,
> shutdown, failures, isolated runtimes, trigger dispatch drains, and provider
> failure context are represented before they are projected into public
> diagnostics and status summaries.

## 1. Lifecycle model

Runtime lifecycle outcomes are represented as structured facts before they are projected into compact public status summaries. Single-value classifications such as `GenerationExitKind`, `ServiceStatus`, and human-readable summaries are too narrow for boundary cases where multiple facts are true at the same time:

- a reload was requested while the generation returned an error;
- a reload was requested while the generation panicked;
- daemon shutdown arrived while trigger dispatches were already in flight;
- an isolated service generation reported an outcome through a bridge, but the backing OS thread was not joined;
- provider resolution failed, but diagnostics only described the resolve shape, not the runtime phase where it happened.

## 2. Design principles

1. **Represent facts, not overwritten conclusions.** Reload, shutdown, panic, provider failure, and trigger failure can be simultaneous facts.
2. **Keep user-facing APIs simple.** Internal records may be richer than public status summaries.
3. **Do not hide failure or panic.** A reload or shutdown signal must not silently erase the underlying generation result.
4. **Do not block shutdown forever.** Graceful shutdown boundaries use bounded waits.
5. **Do not silently discard residual work.** If a bounded wait expires, the residual is recorded as diagnostics.
6. **Separate internal truth from projections.** Diagnostics, status, logs, and docs should be derived from the internal lifecycle record.

## 3. Generation outcome matrix

A generation exit is represented as a structured record with at least these dimensions:

| Dimension | Examples | Purpose |
| :--- | :--- | :--- |
| Generation result | `NormalExit`, `RecoverableError`, `FatalServiceError`, `ProviderInitError`, `TriggerDispatchFailure`, `Panic`, `IsolatedStartupFailure` | What the generation body or bridge actually produced. |
| Lifecycle signals | `reload_requested`, `shutdown_requested` | Which control-plane signals were active while the generation was ending. |
| Restart decision | immediate, backoff by failure kind, no restart | What the supervisor should do next. |
| Daemon action | continue, request shutdown, terminate service | Whether the broader daemon should keep running. |

Illustrative internal shape:

```rust
struct GenerationExitRecord {
    result: GenerationResultKind,
    signals: GenerationSignalFacts,
    restart_decision: RestartDecision,
    daemon_action: DaemonAction,
}

enum GenerationResultKind {
    NormalExit,
    RecoverableError { summary: String },
    FatalServiceError { summary: String },
    ProviderInitError { summary: String },
    TriggerDispatchFailure { summary: String },
    IsolatedStartupFailure { summary: String },
    Panic { summary: String },
}

struct GenerationSignalFacts {
    reload_requested: bool,
    shutdown_requested: bool,
}
```

This shape is internal, not public API. `reload_requested` and `shutdown_requested` are parallel facts instead of overwriting `result`.

Runtime behavior:

- `service-daemon/src/core/service_daemon/runner/supervisor.rs` uses an internal `GenerationExitRecord` with `GenerationResultKind` and `GenerationSignalFacts`.
- Reload is recorded as `signals.reload_requested` instead of overwriting recoverable errors, trigger dispatch failures, or panics.
- Clean exit while reload is requested still projects to reload/restoring semantics.

### 3.1 Reload plus failure

| Scenario | Recorded facts | Projection |
| :--- | :--- | :--- |
| Reload requested, then generation returns `Ok(())` | `result = NormalExit`, `reload_requested = true` | A reload-driven restart may be immediate. |
| Reload requested, then generation returns recoverable error | `result = RecoverableError`, `reload_requested = true` | Diagnostics show the error and the reload request. |
| Reload requested, then generation panics | `result = Panic`, `reload_requested = true` | Diagnostics show the panic and the reload request. |
| Reload requested, then provider init error occurs | `result = ProviderInitError`, `reload_requested = true` | Provider failure remains visible; daemon action follows provider failure semantics. |

The runtime records reload as a signal fact so it does not overwrite recoverable errors, trigger dispatch failures, provider initialization errors, or panics.

### 3.2 HighPriority policy rollover

HighPriority runtime policy rollover uses the reload signal path as a cooperative generation boundary. The policy does not move a running Tokio future between runtime shards. Instead, it records a placement decision, notifies the service's reload signal, and lets the current generation exit at its normal reload-safe point. The next generation resolves providers again and receives a fresh HighPriority shard placement.

This is represented as reload lifecycle, not service failure:

- the exiting generation records reload facts when it observes the signal;
- the replacement generation starts immediately, like other reload-driven generation replacements;
- the rollover does not enter `RestartPolicy` backoff;
- the rollover does not increment restart-storm failure accounting;
- any state that must survive the boundary should use ordinary framework mechanisms such as the Shelf or managed providers.

## 4. Shutdown boundary matrix

Shutdown can encounter work that has already crossed an internal boundary. The runtime should use bounded graceful waits and record residual work.

| Boundary | Graceful action | Timeout result | Required observability |
| :--- | :--- | :--- | :--- |
| Service generation cancel | cancel generation token and wait for generation outcome | generation did not finish before boundary timeout | service, generation, elapsed, residual action |
| Isolated runtime join | wait for bridge outcome, then bounded join the OS thread | thread not joined in time | joined/timed out, bridge closed, panic, runtime build failure |
| Trigger dispatch drain | stop accepting new events, drain in-flight dispatches | in-flight dispatch residual remains | started, completed, failed, timed out, residual |

Illustrative shared projection:

```rust
enum ShutdownBoundary {
    ServiceGenerationCancel,
    IsolatedRuntimeJoin,
    TriggerDispatchDrain,
}

struct ShutdownBoundaryOutcome {
    boundary: ShutdownBoundary,
    graceful_completed: bool,
    timed_out: bool,
    completed: usize,
    residual: usize,
    action: ShutdownResidualAction,
}

enum ShutdownResidualAction {
    None,
    RecordedAndDetached,
    RecordedAndAborted,
    Escalated,
}
```

The shutdown design uses **graceful boundary + observable residual**:

- no indefinite shutdown wait;
- no silent drop;
- no blanket conversion of every shutdown timeout into an ordinary runtime failure;
- residual work is visible in diagnostics snapshots and logs; status summaries may stay compact.

## 5. Isolated service contract

Shutdown contract for isolated service generations:

1. send or observe cancellation;
2. wait for the service generation outcome bridge;
3. bounded-join the backing OS thread;
4. record whether the thread joined, timed out, panicked, failed to build its runtime, or closed the bridge before reporting an outcome.

Runtime behavior:

- `run_isolated_service_generation(...)` stores an internal `IsolatedRuntimeHandle` containing the outcome receiver and OS thread join handle.
- After the outcome bridge resolves, the supervisor attempts a bounded join using the internal default timeout.
- Joined, timed-out, and panicked join outcomes are distinguished internally and projected into the public diagnostics snapshot through bounded shutdown boundary stats.
- Test coverage locks joined, panic, timeout residual helper paths, and public diagnostics projection.

Illustrative internal handle:

```rust
struct IsolatedRuntimeHandle {
    outcome_rx: tokio::sync::oneshot::Receiver<ServiceGenerationOutcome>,
    thread_join: std::thread::JoinHandle<()>,
}
```

This handle should remain internal unless a separate public configuration/API decision is made.

## 6. Trigger shutdown contract

Trigger shutdown contract:

1. enter closing state;
2. stop polling hosts for new events;
3. do not start new dispatches;
4. drain already in-flight dispatches up to a default grace window;
5. record completed, failed, timed out, and residual dispatch counts;
6. continue shutdown without silently losing residual facts.

Runtime behavior:

- `TriggerRunner::run_with_host(...)` enters shutdown drain mode when shutdown arrives while dispatches are in flight.
- Shutdown drain stops polling the host for new events and waits for in-flight dispatches to finish or for the internal default drain timeout.
- The timeout helper returns `TriggerDrainOutcome { completed, failed, timed_out, residual }`, logs residual dispatches, and projects the drain outcome into the public diagnostics snapshot when a generation diagnostics scope is present.
- Test coverage locks completed drain, timeout residual helper paths, public diagnostics projection, and host-specific tail event delivery guarantees.

Illustrative internal outcome:

```rust
struct TriggerDrainOutcome {
    started: usize,
    completed: usize,
    failed: usize,
    timed_out: bool,
    residual: usize,
}
```

Host-specific delivery guarantees may still differ. Queue, Watch, Cron, and Signal hosts should document whether unaccepted tail events are best-effort, dropped, or externally retained.

### 6.1 Host-specific tail event guarantees

The shutdown drain boundary only covers dispatches that the runner has already accepted and started. Once shutdown is observed, the runner stops polling `handle_step`; events that remain inside the host's underlying mechanism are governed by that mechanism:

| Host | Underlying source | Shutdown tail-event guarantee |
| :--- | :--- | :--- |
| `TopicHost` / `Queue` | `tokio::sync::broadcast::Receiver` subscribed during `setup()` | In-flight dispatches are drained by the runner. Messages not yet received by this trigger generation remain only in the receiver's broadcast buffer while the generation is alive; when the generation exits, its receiver is dropped. Later generations create a new receiver and do not replay messages that were only buffered for the old receiver. Lagged messages are already best-effort and recorded as skipped by `TopicHost`. |
| `SignalHost` / `Notify` | `TrackedNotify` backed by `tokio::sync::Notify` | Notifications are edge-style wakeups, not a durable queue. A notification already accepted by the runner becomes an in-flight dispatch and is drained; pending or concurrent notifications not accepted before shutdown are best-effort and may coalesce or be lost with the exiting generation. |
| `CronHost` | shared `tokio-cron-scheduler` callback bridged through `Notify` | Cron ticks accepted by the runner become in-flight dispatches and are drained. Ticks that arrive after shutdown starts are not polled by the exiting generation; the scheduler may continue running process-wide, but the exiting generation's notify bridge is dropped, so missed ticks are not replayed. |
| `WatchHost` / `State` | generation-scoped provider dependency watch | The first watch dispatch in a generation is accepted and then the trigger idles for reload. Provider changes are level-triggered through the daemon's generation dependency watch path, so the next generation captures a fresh baseline rather than relying on queued tail events from the old generation. |

Custom `TriggerHost` implementations should document whether unaccepted events are durable, best-effort, or externally retained. To obtain drain guarantees, the host must return `TriggerTransition::Next` or `Reload` before shutdown is observed; after that point the runner owns the in-flight dispatch and applies the bounded drain contract.

## 7. Provider failure context

Provider failures need two independent dimensions:

| Dimension | Examples | Answers |
| :--- | :--- | :--- |
| Runtime phase | `StartupEagerInit`, `ServiceGenerationResolve`, `ReloadGenerationResolve`, `TriggerDispatchResolve`, `FrameworkValidation` | Where in the lifecycle did this happen? |
| Resolve boundary | `SnapshotResolve`, `RwLockResolve`, `MutexResolve`, `EagerInit` | Which provider access path failed? |
| Source kind | `user_provider_fatal`, `user_provider_retryable_timeout`, `environment_missing`, `panic`, `cancelled`, ... | What produced the failure? |

Illustrative internal shape:

```rust
enum ProviderRuntimePhase {
    StartupEagerInit,
    ServiceGenerationResolve,
    ReloadGenerationResolve,
    TriggerDispatchResolve,
    FrameworkValidation,
}

enum ProviderResolveBoundary {
    SnapshotResolve,
    RwLockResolve,
    MutexResolve,
    EagerInit,
}

struct ProviderFailureContext {
    provider: &'static str,
    phase: ProviderRuntimePhase,
    boundary: ProviderResolveBoundary,
    source: ProviderInitSourceKind,
}
```

Runtime behavior:

- `ProviderInitBoundaryContext` stores provider name, runtime phase, and resolve boundary.
- Runtime phase is supplied by an internal task-local scope so user service, trigger, and provider signatures do not change.
- Service generation provider failures default to `ServiceGenerationResolve`; if the generation reload token is already cancelled when the failure is recorded, the phase is upgraded to `ReloadGenerationResolve`.
- Trigger dispatch tasks run under `TriggerDispatchResolve`.
- Eager provider initialization runs under `StartupEagerInit`; provider graph validation failures use `FrameworkValidation`.
- `trace_provider_init_failure(...)` emits structured fields for provider, runtime phase, resolve boundary, source kind, failure kind, and error.
- Retryable provider timeouts retain internal retry diagnostics: attempts, elapsed time, last retry delay, and a bounded recent-error list.
- Public diagnostics snapshots project provider failure context through daemon-level recent provider failures and service/generation/lane aggregate counters. The status plane intentionally remains compact (`ProviderInitError` level summary) instead of duplicating the full provider failure context.

## 8. Projection rules

Internal lifecycle records are the source of truth. Other surfaces are projections:

```text
Internal lifecycle record
    -> diagnostics event/snapshot
    -> status plane summary
    -> tracing/log messages
    -> human documentation
```

Rules:

- human-readable summaries must not be the only stored fact;
- `ServiceStatus::Recovering(String)` should not become the state-machine truth source;
- diagnostics should retain enough structured fields to distinguish reload+panic from clean reload;
- status summaries may stay compact, but must not erase panic/failure/residual facts.

## 9. Validation coverage

Runtime contract tests assert:

- reload + recoverable error preserves both `reload_requested` and `RecoverableError`;
- reload + panic preserves both `reload_requested` and `Panic`;
- reload + trigger dispatch failure preserves both `reload_requested` and the dispatch failure classification;
- isolated bounded join reports joined, panic, and timeout residual helper paths;
- isolated join timeout/panic and trigger shutdown drain outcomes are projected into public diagnostics snapshot boundary stats;
- trigger shutdown drain reports completed drain and timeout residual helper paths;
- provider failure context public diagnostics projection covers eager fatal, runtime retry timeout, trigger dispatch provider failure, and reload generation provider failure.

## 10. Open decisions

- Exact internal Rust type names remain internal. Current lifecycle helpers live under `service_daemon::{builder,provider_graph,runtime,startup_pipeline,runner::{supervisor,generation,wave}}` and `trigger_runner::{event_loop,dispatch,drain,scaling,interceptors,failure,message_id}`; the `service_daemon::mod` file remains the public daemon facade.
- Whether timeout values become public configuration.
- Which shutdown residual actions are purely diagnostic and which should escalate.
- Whether any future status-plane expansion is needed beyond the current compact lifecycle summaries.
