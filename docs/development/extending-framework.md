# Extending the Framework

This guide is for developers looking to add new capabilities to the `service-daemon-rs` core or macros.

## 1. Adding a New Trigger Template

Triggers are implemented as stateful hosts with a two-phase lifecycle managed by the **Policy-Engine separation** model.

1. **Define the Host**: Add a struct in `service-daemon/src/core/triggers.rs`. It can hold initialized resources as fields.
2. **Implement `TriggerHost<T>`**:
   - **`setup(target: Arc<T>)`**: Called once when the trigger service starts. Use this to acquire resources (subscribers, scheduler jobs, etc.) and return an initialized `Self`.
   - **`handle_step(&mut self, target: &Arc<T>)`**: Called in each event loop iteration. Define the waiting logic and return a `TriggerTransition`.
   - **`scaling_policy()`** (Optional): Override to enable elastic scaling (e.g., for queues).
3. **Register Aliases**: Add short aliases to the `TT` module in `service-daemon/src/models/trigger.rs`.
4. **Update the Macro** (Optional): Modify `service-daemon-macro/src/trigger/parser.rs` and `codegen.rs` if you want specialized attribute syntax like `#[trigger(MyTemplate(...))]`.

> [!NOTE]
> The `#[trigger]` macro calls `TriggerHost::run_as_service` by default. This default implementation automatically handles the `setup` -> `TriggerRunner` lifecycle. Most hosts do **not** need to override `run_as_service`.

### Example: Custom Host with Scaling

```rust
pub struct MyHost {
    rx: tokio::sync::mpsc::Receiver<String>,
}

impl<T> TriggerHost<T> for MyHost
where
    T: Provided + Send + Sync + 'static,
{ // NOTE: for Watch-style triggers, prefer `T: WatchableProvided`.
    type Payload = String;

    fn setup(_target: Arc<T>) -> BoxFuture<'static, anyhow::Result<Self>> {
        Box::pin(async {
            let (tx, rx) = tokio::sync::mpsc::channel(100);
            // Initialize your event source here...
            Ok(MyHost { rx })
        })
    }

    fn handle_step<'a>(&'a mut self, _target: &'a Arc<T>)
        -> BoxFuture<'a, TriggerTransition<Self::Payload>>
    {
        Box::pin(async move {
            match self.rx.recv().await {
                Some(msg) => TriggerTransition::Next(msg),
                None => TriggerTransition::Stop,
            }
        })
    }

    fn scaling_policy() -> Option<ScalingPolicy> {
        Some(ScalingPolicy::default())
    }
}
```

## 2. Adding a "Provider Template"

Provider templates (like `Notify` or `Queue`) generate specialized struct bodies and convenience methods automatically.

> [!IMPORTANT]
> **Stop!** Do not add a Template for business-specific components (e.g., MQTT, Database). Instead, use a regular `#[provider]` on an `async fn`.
> 
> Only add a Template if you are introducing a **generic architectural primitive** that requires specialized code-generation.

1. Add a new template generator function in `service-daemon-macro/src/provider/templates.rs`.
2. Update the `TEMPLATE_NAMES` list in `service-daemon-macro/src/provider/parser.rs`.
3. Update `try_generate_template` (or equivalent logic) in `service-daemon-macro/src/provider/struct_gen.rs` to wire up your new generator.

## 3. Adding Custom Interceptors

The `TriggerInterceptor<P>` trait provides a composable, onion-model middleware layer.

1. **Implement `TriggerInterceptor<P>`**: Define `intercept(ctx, next)`.
2. **Registration**: Registered via `TriggerRunner::with_interceptor()`.
3. **Flow**: Interceptors execute in registration order. You decide when to call `next(ctx).await`.

```rust
pub struct MyInterceptor;

impl<P: Send + Sync + 'static> TriggerInterceptor<P> for MyInterceptor {
    fn intercept<'a>(
        &'a self,
        ctx: DispatchContext<P>,
        next: Next<'a, P>,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let start = std::time::Instant::now();
            let result = next(ctx).await;
            tracing::info!("Dispatch took {:?}", start.elapsed());
            result
        })
    }
}
```

## 4. Modifying Registry Mechanics

The framework uses `linkme` for distributed registration and `ServiceId` for identity.

1. **Identity**: `ServiceId` values are assigned at compile-time via `linkme`.
2. **Context**: Use `current_service_id()` from `service-daemon/src/models/trigger.rs` to retrieve the ID of the running service.
3. **Macro Generation**: Shared logic resides in `service-daemon-macro/src/common.rs`.
    - **`decompose_type`**: This utility is key to the DI system. It recursively inspects AST types to identify wrappers like `Arc`, `RwLock`, and `Mutex`, stripping the outer layers to reach the inner `T`.
    - **`ParamIntent`**: Parameters are categorized as either `Payload` or `Dependency`. This allows a single function to mix event data with DI-resolved resources seamlessly.

[Back to README](../../README.md)
