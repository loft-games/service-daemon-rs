# `#[trigger]` reference

## 1. Syntax and attributes

```
#[trigger(Host(Target))]
#[trigger(Host(Target), priority = N, scheduling = Isolated, tags = ["bg"])]
```

- `Host(Target)` is a real Rust type path (the host alias resolved against
  `TriggerHost<Target>`), not a magic keyword — `Target` is the `#[provider]`
  type that supplies the event source.
- Optional `priority` / `scheduling` / `tags` behave exactly as for `#[service]`
  (priority orders startup waves; scheduling is `Standard`/`HighPriority`/`Isolated`).

## 2. Host families

| Host family aliases | Backing host | Fires when | Target |
| :--- | :--- | :--- | :--- |
| `Cron(T)` | `CronHost` | cron schedule ticks | provider yielding the cron expression `String` |
| `Queue(T)`, `BQueue(T)`, `BroadcastQueue(T)` | `TopicHost` | an item is published to the broadcast queue | `#[provider(Queue(T))]` |
| `Event(T)`, `Notify(T)`, `Signal(T)`, `Custom(T)` | `SignalHost` | a signal/notification is raised | `#[provider(Notify)]` |
| `Watch(T)`, `State(T)` | `WatchHost` | the target provider's managed state changes | any provider (managed/watchable) |

## 3. Handler signature: payload vs dependencies

For triggers, a non-`Arc` parameter is the **payload**. Services have their own
separate non-`Arc` exception, `#[input] cfg: &Cfg`, for on-demand service
templates; do not use trigger payload rules for services.

```rust
// Queue: the published item is passed by value (auto-cloned per subscriber).
#[trigger(Queue(TaskQueue))]
async fn on_item(item: String) -> anyhow::Result<()> { Ok(()) }

// Watch: the changed snapshot is passed as Arc<T>.
#[trigger(Watch(MetricsData), priority = 80)]
async fn on_change(snapshot: Arc<MetricsData>) -> anyhow::Result<()> { Ok(()) }

// Cron / Event: no payload.
#[trigger(Cron(CleanupSchedule))]
async fn on_tick() -> anyhow::Result<()> { Ok(()) }

// Extra dependencies are injected as Arc<T>, alongside any payload.
#[trigger(Queue(TaskQueue))]
async fn on_item_with_dep(item: String, db: Arc<DbPool>) -> anyhow::Result<()> { Ok(()) }
```

Rules:

- Queue handlers: payload by value (`String`, `ComplexJob`, …). Matches the
  `Queue(T)` provider's payload type.
- Watch handlers: `Arc<T>` snapshot of the watched type.
- Cron / Event / Notify / Signal handlers: no payload.
- Any number of additional `#[provider]` dependencies, each `Arc<T>`.
- Return `anyhow::Result<()>`.

## 4. Supervision semantics

- A handler `Err(..)` is retried by the trigger runner; retry exhaustion and
  dispatch-infrastructure errors are bridged to the supervisor as recoverable
  generation failures. Dispatch panics keep panic classification (same backoff
  behavior as service panics).
- Triggers are `ServiceDescription` entries, so a declared `HighPriority` trigger
  contributes to runtime lane sizing exactly like a service.

HighPriority requires the dependency's `high-priority` Cargo feature; without it,
the declaration fails to compile. Trigger-host `ScalingPolicy` and policy overlays
govern dispatch concurrency/retry behavior, not HighPriority runtime shard scaling.
The initial runtime feedback path uses completed framework-aware ServiceSleep
observations with supporting shard pressure, not trigger backlog or handler
throughput. Do not add artificial sleeps to a handler for policy input. Authors
remain responsible for in-flight business work across reload.

## 5. Extending hosts (advanced)

The built-in hosts are `SignalHost`, `TopicHost`, `CronHost`, `WatchHost`. The
internal interceptor/middleware pipeline (`TriggerInterceptor`) is **not** a public
registration API. The supported extension point for custom event sources is
implementing `TriggerHost<T>` yourself — see the `sd-macro-development` skill and
the framework's maintainer docs for that path.
