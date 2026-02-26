# Extending the Framework

This guide is for developers looking to add new capabilities to the `service-daemon-rs` core or macros.

## 1. Adding a New Trigger Template

Triggers are implemented as stateful hosts with a two-phase lifecycle: **setup** (one-time initialization) and **handle_step** (per-event waiting).

1. **Define the Host**: Add a struct in `service-daemon/src/core/triggers.rs`. It can be a unit struct (zero-sized) if no state is needed, or hold initialized resources as fields.
2. **Implement `TriggerHost<T>`**:
   - **`setup(target: Arc<T>)`**: Called once when the trigger service starts. Use this to acquire resources (subscribers, scheduler jobs, etc.) and return an initialized `Self`.
   - **`handle_step(&mut self, target: &Arc<T>)`**: Called in each event loop iteration. Define the waiting logic and return a `TriggerTransition`. Since `self` is mutable, you can access and modify state stored during `setup`.
3. **Register Aliases**: Add short aliases to the `TT` module in `service-daemon/src/models/trigger.rs`.
4. **Update the Macro** (optional): Modify `service-daemon-macro/src/trigger/codegen.rs` to recognize the new template name if you want `#[trigger(MyTemplate(...))]` syntax. Update `trigger/parser.rs` if new attributes are needed.
5. **Map Parameters**: Use the macro utilities in `trigger/mod.rs` to correctly distinguish between event payloads and DI resources.

> [!NOTE]
> The `#[trigger]` macro calls `TriggerHost::run_as_service`, whose default implementation handles `setup` → `TriggerRunner::run_with_host` automatically. Most custom hosts do **not** need to override `run_as_service`.

### Example: Minimal Custom Host

```rust
pub struct MyHost;

impl<T> TriggerHost<T> for MyHost
where
    T: Provided + Send + Sync + 'static,
{
    type Payload = ();

    fn setup(_target: Arc<T>) -> BoxFuture<'static, anyhow::Result<Self>> {
        Box::pin(async { Ok(MyHost) })
    }

    fn handle_step<'a>(&'a mut self, target: &'a Arc<T>)
        -> BoxFuture<'a, TriggerTransition<Self::Payload>>
    {
        Box::pin(async move {
            // Your custom event-waiting logic here
            TriggerTransition::Next(())
        })
    }
}
```

## 2. Adding a "Magic Provider"

Magic providers (like `Notify` or `Queue`) provide specialized behavior automatically when used as a default.

> [!IMPORTANT]
> **Stop!** Do not add a Magic Provider for business-specific components (e.g., MQTT, Database, Redis). Instead, use a regular `#[provider]` on an `async fn` in your application code. This is easier to maintain and provides full control over initialization.
> 
> Only add a "Magic Provider" if you are introducing a **generic architectural primitive** that requires specialized code-generation (like automatically adding convenience methods via macro).

1. Add a new template generator function in `service-daemon-macro/src/provider/templates.rs`.
2. Update `generate_struct_provider` in `service-daemon-macro/src/provider/struct_gen.rs` to detect your new template name in the `#[provider(default = ...)]` attribute.

## 3. Adding Custom Middleware

The `TriggerMiddleware` trait allows pluggable hooks around each trigger dispatch cycle.

1. Implement `TriggerMiddleware` with `before_dispatch` and `after_dispatch` methods.
2. Register your middleware via `TriggerRunner::with_middleware()`.
3. Middlewares execute in registration order for `before_dispatch` and in **reverse** order for `after_dispatch` (onion model).

> [!NOTE]
> The current middleware trait is an **observer pattern** — middleware can inspect but not control the dispatch. A future **interceptor pattern** (Tower-like chaining) is documented in `docs/future_interceptor_middleware.md`.

## 4. Modifying Registry Mechanics

The framework uses `linkme` for distributed registration. If you need to change how services are registered:
1. Update common types in `service-daemon/src/models/`.
2. Ensure consistent updates in the macro generation logic in `service-daemon-macro/src/service/codegen.rs` and `trigger/codegen.rs`.


[Back to README](../../README.md)
