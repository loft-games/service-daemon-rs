# Custom Trigger Hosts

> [!NOTE]
> This is an advanced extension guide, not part of the beginner quick-start path.

The framework comes with built-in triggers like `Queue`, `Cron`, and `Watch` (State).

> [!NOTE]
> `Watch(T)` requires the target type to implement `WatchableProvided`. (Note: This is automatically handled by the `#[provider]` macro.)

Real systems often need more: a GPIO pin interrupt, an HTTP webhook, a vendor sensor protocol, a filesystem watch.

To create a custom trigger, you implement the **`TriggerHost<T>`** trait.

---

## 1. The Policy vs. Engine Model

Triggers are split into two parts:

1. **Engine (Framework)**: The `TriggerRunner` handles the infinite loop, interceptor pipeline (tracing, retry with backoff), standard shutdown logic, and **optional concurrent dispatch** -- dispatching handlers asynchronously via `tokio::spawn` with semaphore-gated concurrency, enabled only when the template declares a `ScalingPolicy` via `TriggerHost::scaling_policy()`.
2. **Policy (Your Host)**: Defines only *how to initialize* (`setup`) and *how to wait* for the next event (`handle_step`).


### Why `Clone` for Payloads?

The framework wraps every payload in `Arc<P>` internally so that retries only clone a pointer. If your handler receives a **bare `T`**, the framework must clone the data out of the `Arc` -- so `T` must implement `Clone`. If your handler receives `Arc<T>`, no cloning happens at all.

> [!TIP]
> **What if my data isn't `Clone`?**
> If your payload is large or cannot implement `Clone`, keep `type Payload = MyData` and declare the handler parameter as `#[payload] data: Arc<MyData>`.
> Retries then clone only the shared pointer, not the underlying data.

The split lets you focus on the event-waiting logic; the framework reuses one engine across every trigger type.

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

### Declaring queue concurrency

By default, custom triggers dispatch events **serially**. If your trigger is a streaming event source that benefits from concurrent handler execution, override `scaling_policy()`:

```rust,ignore
fn scaling_policy() -> Option<ScalingPolicy> {
    Some(ScalingPolicy::default())
}
```

This enables pressure-based concurrency adjustment (`scale_monitor`). Users can further override your defaults via `ServiceDaemonBuilder::with_trigger_config(ScalingPolicy::builder()...build())`.

## 3. Overriding the service loop

If `handle_step` is not enough, for example because an integration has specific threading requirements or needs full control over the execution loop, you can override `run_as_service`.

```rust,ignore
impl<T> TriggerHost<T> for MyCustomHost {
    // ...
    fn run_as_service(
        name: String,
        target: Arc<T>,
        handler: TriggerHandler<Self::Payload>,
        token: CancellationToken, // The framework's shutdown signal
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async move {
            // Custom loop owns dispatch and shutdown checks.
            // Implement tracing manually if this loop needs it.
            while !token.is_cancelled() {
                // ... logic ...
            }
            Ok(())
        })
    }
}
```

> [!CAUTION]
> If you override the service loop, you lose the framework's automatic traceability (monotonically increasing IDs, tracing spans), interceptor pipeline, and retry logic unless you implement them manually. Use this only when `handle_step` cannot represent the integration.

---

[Back to Event Triggers](../triggers.md)
