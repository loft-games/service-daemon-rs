# Tailor-Made Triggers

The framework comes with built-in triggers like `Queue`, `Cron`, and `Watch` (State). But world-class systems often need more—like a GPIO pin interrupt, an HTTP webhook, or a proprietary sensor protocol.

To create a custom trigger, you implement the **`TriggerHost<T>`** trait.

---

## 1. The Policy vs. Engine Model

Starting from v0.1.0, triggers are split into two parts:
1.  **Engine (Framework)**: Handles the infinite loop, tracing, monotonically increasing instance IDs, recovery, and standard shutdown logic.
2.  **Policy (Your Host)**: Defines only *how to wait* for the next event.

### Why `Clone` for Payloads?
The framework wraps every payload in `Arc<P>` internally so that retries only clone a pointer. If your handler receives a **bare `T`**, the framework must clone the data out of the `Arc` — so `T` must implement `Clone`. If your handler receives `Arc<T>`, no cloning happens at all.

> [!TIP]
> **What if my data isn't `Clone`?**
> If your payload is large or cannot implement `Clone`, wrap it in an `Arc`: `type Payload = Arc<MyData>`, and declare your handler parameter as `Arc<Arc<MyData>>` or simply use `#[payload] data: Arc<MyData>`. 
> Since `Arc` itself is always `Clone`, the retry mechanism will work perfectly without touching the underlying data.

This decoupled design means you spend zero time on boilerplate and focus entirely on the event-waiting logic.

## 2. Implementing a Custom Trigger

Let's imagine you want a trigger that fires whenever a file is created.

```rust
use service_daemon::{TriggerHost, TriggerTransition, Provided};
use service_daemon::futures::future::BoxFuture;
use std::sync::Arc;
use std::path::PathBuf;

pub struct FileWatcherHost;

impl<T> TriggerHost<T> for FileWatcherHost 
where 
    T: Provided + std::ops::Deref<Target = PathBuf> + Send + Sync + 'static 
{
    /// **Payload**: Must implement `Clone` to support automatic retries.
    type Payload = String; 

    /// **Policy**: Define how to wait for the next event.
    fn handle_step(target: Arc<T>) -> BoxFuture<'static, TriggerTransition<Self::Payload>> {
        Box::pin(async move {
            // Your custom event logic (e.g., using the `notify` crate)
            match wait_for_file_system_event(&target).await {
                Ok(filename) => TriggerTransition::Next(filename),
                Err(_) => TriggerTransition::Stop,
            }
        })
    }
}
```

### The `TriggerTransition` Protocol
Your `handle_step` method returns a instruction to the engine:
*   `TriggerTransition::Next(payload)`: Dispatch event and loop immediately.
*   `TriggerTransition::Reload(payload)`: Dispatch event, then wait for a framework restart (ideal for state-watchers).
*   `TriggerTransition::Stop`: Cleanly exit the loop.

## 3. Handling Continuity (Shelving)

What if your trigger has state that must persist across `handle_step` calls (like a network connection or a library's `Receiver`)? Use the **Shelf**!

```rust
use service_daemon::context::{shelve, shelve_clone};

// Inside handle_step...
let rx = match shelve_clone::<MyRx>("bridge").await {
    Some(rx) => rx,
    None => {
        let rx = create_new_rx();
        shelve("bridge", rx.clone()).await;
        rx
    }
};
```

By leveraging `shelve_clone`, your trigger remains stateless in its signature but maintains perfect continuity in its execution.

## 4. The Ultimate Escape Hatch: `run_as_service`

Sometimes, `handle_step` is simply not enough. If you’re integrating a legacy C library with weird threading requirements, or a high-performance system that requires full control over the execution loop, you can override the **`run_as_service`** engine itself.

```rust
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
> **With great power comes great responsibility.** If you override the engine, you lose the framework's automatic traceability (monotonically increasing IDs, tracing spans) unless you implement them manually using `TriggerContext` and `Instrument`. Use this only as a last resort!

---

[**← Previous Step: Under the Hood**](under-the-hood.md) | [**Next Step: Macro Magic Unleashed →**](macro-magic.md)
