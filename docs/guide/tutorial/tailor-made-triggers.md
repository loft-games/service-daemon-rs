# Tailor-Made Triggers

The framework comes with built-in triggers like `Queue`, `Cron`, and `Watch` (State). But world-class systems often need more—like a GPIO pin interrupt, an HTTP webhook, or a proprietary sensor protocol.

To create a custom trigger, you implement the **`TriggerHost`** trait.

---

## 1. What is a Trigger Host?

A `TriggerHost` is a specialized service that acts as a bridge between an external event source and the framework's handler functions. It defines:
1.  **The Target**: The configuration or event-source provider (resolved via DI).
2.  **The Payload**: The data type passed to the handler function.
3.  **The Loop**: An asynchronous task that waits for events and invokes the handler.

## 2. Implementing a Custom Trigger

Let's imagine you want a trigger that fires whenever a file is created.

```rust
use service_daemon::{TriggerHost, TriggerHandler, TriggerContext, TriggerMessage, ServiceId, Provided};
use service_daemon::futures::future::BoxFuture;
use tokio_util::sync::CancellationToken;
use std::path::PathBuf;

pub struct FileWatcherHost;

impl TriggerHost for FileWatcherHost {
    type Target = PathBuf; // The directory to watch, resolved from DI
    type Payload = String; // We send the filename as the payload

    fn run_as_service(
        name: String,
        target: Self::Target,
        handler: TriggerHandler<Self::Payload>,
        token: CancellationToken,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async move {
            tracing::info!("Starting FileWatcherHost on {:?}", target);
            
            // Your custom event loop (e.g., using the `notify` crate)
            loop {
                tokio::select! {
                    _ = token.cancelled() => break,
                    event = some_external_event() => {
                        // Construct a Context with traceability
                        let ctx = TriggerContext {
                            service_id: ServiceId::new(&name),
                            instance_seq: 0, 
                            message: TriggerMessage {
                                message_id: service_daemon::generate_message_id(),
                                source_id: ServiceId::new(&name),
                                timestamp: chrono::Utc::now(),
                                payload: event,
                            },
                        };
                        // Fire the handler!
                        let _ = handler(ctx).await;
                    }
                }
            }
            Ok(())
        })
    }
}
```

> [!IMPORTANT]
> **Macro Limitation**: While the `TriggerHost` trait is fully extensible, the current `#[trigger]` macro is optimized for built-in variants. For full custom hosts, you might need to register the host as a standard `#[service]` that manually invokes a handler, or use the `Custom` template variant if applicable.

## 3. The Power of Traceability

By implementing a custom host, you gain full access to the framework's **Ripple Model**. Every `TriggerMessage` you fire includes a `message_id` and `source_id`, ensuring that your custom events are just as traceable as a built-in Cron job or Message Queue.

---

[**← Previous Step: Under the Hood**](under-the-hood.md) | [**Next Step: Macro Magic Unleashed →**](macro-magic.md)
