# Tailor-Made Triggers

The framework comes with built-in triggers like `Queue`, `Cron`, and `Watch` (State).

> [!NOTE]
> `Watch(T)` requires the target type to implement `WatchableProvided`.

But world-class systems often need more--like a GPIO pin interrupt, an HTTP webhook, or a proprietary sensor protocol.

To create a custom trigger, you implement the **`TriggerHost<T>`** trait.

---

## 1. The Policy vs. Engine Model

Triggers are split into two parts:

1. **Engine (Framework)**: The `TriggerRunner` handles the infinite loop, interceptor pipeline (tracing, retry with backoff), standard shutdown logic, and **conditional elastic scaling** -- dispatching handlers asynchronously via `tokio::spawn` with semaphore-gated concurrency, enabled only when the template declares a `ScalingPolicy` via `TriggerHost::scaling_policy()`.
2. **Policy (Your Host)**: Defines only *how to initialize* (`setup`) and *how to wait* for the next event (`handle_step`).


### Why `Clone` for Payloads?

The framework wraps every payload in `Arc<P>` internally so that retries only clone a pointer. If your handler receives a **bare `T`**, the framework must clone the data out of the `Arc` -- so `T` must implement `Clone`. If your handler receives `Arc<T>`, no cloning happens at all.

> [!TIP]
> **What if my data isn't `Clone`?**
> If your payload is large or cannot implement `Clone`, wrap it in an `Arc`: `type Payload = Arc<MyData>`, and declare your handler parameter as `Arc<Arc<MyData>>` or simply use `#[payload] data: Arc<MyData>`.
> Since `Arc` itself is always `Clone`, the retry mechanism will work perfectly without touching the underlying data.

This decoupled design means you spend zero time on boilerplate and focus entirely on the event-waiting logic.

## 2. Implementing a Custom Trigger

Let's imagine you want a trigger that fires whenever a file is created.

### Stateless Host (No Initialization Needed)

```rust,ignore
use service_daemon::{TriggerHost, TriggerTransition, Provided, WatchableProvided};
use service_daemon::futures::future::BoxFuture;
use std::sync::Arc;
use std::path::PathBuf;

pub struct FileWatcherHost;

impl<T> TriggerHost<T> for FileWatcherHost 
where 
    T: Provided + std::ops::Deref<Target = PathBuf> + Send + Sync + 'static 
{
    type Payload = String;

    fn setup(_target: Arc<T>) -> BoxFuture<'static, anyhow::Result<Self>> {
        Box::pin(async { Ok(FileWatcherHost) })
    }

    fn handle_step<'a>(&'a mut self, target: &'a Arc<T>)
        -> BoxFuture<'a, TriggerTransition<Self::Payload>>
    {
        Box::pin(async move {
            match wait_for_file_system_event(&target).await {
                Ok(filename) => TriggerTransition::Next(filename),
                Err(_) => TriggerTransition::Stop,
            }
        })
    }
}
```

### Stateful Host (With Initialization)

If your trigger needs to set up resources (like a network connection or scheduler job), do it in `setup` and store them as struct fields:

```rust,ignore
pub struct WebSocketHost {
    connection: WebSocketConnection,
}

impl<T> TriggerHost<T> for WebSocketHost
where
    T: Provided + std::ops::Deref<Target = String> + Send + Sync + 'static,
{
    type Payload = Message;

    fn setup(target: Arc<T>) -> BoxFuture<'static, anyhow::Result<Self>> {
        Box::pin(async move {
            let conn = WebSocketConnection::connect(&*target).await?;
            Ok(WebSocketHost { connection: conn })
        })
    }

    fn handle_step<'a>(&'a mut self, _target: &'a Arc<T>)
        -> BoxFuture<'a, TriggerTransition<Self::Payload>>
    {
        Box::pin(async move {
            // Access initialized resources directly via self
            match self.connection.next_message().await {
                Ok(msg) => TriggerTransition::Next(msg),
                Err(_) => TriggerTransition::Stop,
            }
        })
    }
}
```

> [!TIP]
> The `setup` -> `handle_step(&mut self)` pattern eliminates the need for `shelve`-based state persistence in most cases. Resources initialized in `setup` are available as struct fields in every `handle_step` call.

### The `TriggerTransition` Protocol
Your `handle_step` method returns an instruction to the engine:
*   `TriggerTransition::Next(payload)`: Dispatch event and loop immediately.
*   `TriggerTransition::Reload(payload)`: Dispatch event, then wait for a framework restart (ideal for state-watchers).
*   `TriggerTransition::Stop`: Cleanly exit the loop.

### Declaring Elastic Scaling

By default, custom triggers dispatch events **serially** (no scaling overhead). If your trigger is a streaming event source that benefits from concurrent handler execution, override `scaling_policy()`:

```rust,ignore
fn scaling_policy() -> Option<ScalingPolicy> {
    Some(ScalingPolicy::default())
}
```

This enables the framework's pressure-based auto-scaler (`scale_monitor`). Users can further override your defaults via `ServiceDaemonBuilder::with_trigger_config(ScalingPolicy::builder()...build())`.

## 3. The Ultimate Escape Hatch: `run_as_service`

Sometimes, `handle_step` is simply not enough. If you're integrating a legacy C library with weird threading requirements, or a high-performance system that requires full control over the execution loop, you can override the **`run_as_service`** engine itself.

```rust,ignore
impl<T> TriggerHost<T> for MyUltimateHost {
    // ...
    fn run_as_service(
        name: String,
        target: Arc<T>,
        handler: TriggerHandler<Self::Payload>,
        token: CancellationToken, // The framework's shutdown signal
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async move {
            // YOU are now the engine.
            // You must handle your own loop, tracing, and shutdown checks.
            while !token.is_cancelled() {
                // ... logic ...
            }
            Ok(())
        })
    }
}
```

> [!CAUTION]
> **With great power comes great responsibility.** If you override the engine, you lose the framework's automatic traceability (monotonically increasing IDs, tracing spans), interceptor pipeline, and retry logic unless you implement them manually. Use this only as a last resort!

---

[**<- Previous Step: Under the Hood**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/under-the-hood.md) | [**Next Step: The Interceptor Gauntlet ->**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/interceptor-gauntlet.md)
