# Tailor-Made Triggers

The framework comes with built-in triggers like `Queue` and `Cron`. But what if you need to trigger a service based on a custom event source—like a GPIO pin interrupt, an HTTP webhook, or a proprietary sensor protocol?

You can implement your own by implementing the `TriggerTemplate` trait.

---

## 1. What is a Trigger Template?

A Trigger is essentially a factory. It defines:
1.  **The Payload**: What data is passed to the handler function.
2.  **The Host**: The background loop that waits for the event and calls the handler.

## 2. Implementing a Custom Trigger

Let's imagine you want a trigger that fires whenever a file is created in a directory.

```rust
use service_daemon::{TriggerTemplate, TriggerHost, TriggerContext};
use async_trait::async_trait;

pub struct FileWatcher(pub PathBuf);

#[async_trait]
impl TriggerTemplate for FileWatcher {
    async fn run_host(&self, ctx: TriggerContext) -> anyhow::Result<()> {
        // ctx contains the handler function you defined with #[trigger]
        let mut watcher = notify::recommended_watcher(move |res| {
            if let Ok(event) = res {
                // When an event happens, fire the handler!
                ctx.fire(event).await; 
            }
        })?;

        watcher.watch(&self.0, RecursiveMode::NonRecursive)?;
        
        // Keep the host alive until shutdown
        ctx.wait_shutdown().await;
        Ok(())
    }
}
```

## 3. Usage

Once implemented, you can use your custom trigger exactly like a built-in one:

```rust
#[trigger(FileWatcher("/tmp/uploads".into()))]
async fn on_new_file(event: notify::Event) -> anyhow::Result<()> {
    tracing::info!("File changed: {:?}", event.paths);
    Ok(())
}
```

## 4. Pro Tip: Parameter Mapping

The `ctx.fire(payload)` method is intelligent. It maps the payload to the first non-DI argument of your function. If your handler needs access to other Providers (like a database), the framework will inject them automatically before calling your `on_new_file` function.

---

[**← Previous Step: Under the Hood**](under-the-hood.md) | [**Next Step: Macro Magic Unleashed →**](macro-magic.md)
