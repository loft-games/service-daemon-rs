//! Service Daemon Macro Crate
//!
//! This crate provides procedural macros for the `service-daemon` library:
//! - `#[service]` - Mark functions as managed services
//! - `#[trigger]` - Event-driven trigger functions
//! - `#[provider]` - Dependency injection providers
//! - `#[allow(sync_handler)]` - Suppress warnings for intentionally synchronous functions

use proc_macro::TokenStream;
use proc_macro_error2::proc_macro_error;

mod common;
mod provider;
mod service;
mod trigger;

// Internal Module Structure:
// - trigger/: Macro logic for #[trigger]. Split into mod.rs (main), parser.rs (attributes), and codegen.rs (logic).
// - service/: Macro logic for #[service]. Split into mod.rs (main) and codegen.rs (helpers).
// - provider/: Macro logic for #[provider]. Split into mod.rs, parser.rs, templates.rs (special types), and struct_gen.rs (DI).

/// Marks a function as a service managed by ServiceDaemon.
///
/// The macro automatically registers the service in the global registry
/// using `linkme` - no build.rs or manual registration needed!
///
/// The macro generates:
/// 1. A wrapper function that resolves dependencies from the global container
/// 2. A static registry entry that is automatically collected at link time
///
/// > [!IMPORTANT]
/// > **Async Preferred**: Always prefer `async fn` for services. Synchronous
/// > functions will trigger a runtime warning unless annotated with
/// > `#[allow(sync_handler)]`.
///
/// # Example
/// ```rust,ignore
/// use service_daemon::service;
/// use std::sync::Arc;
///
/// #[service]
/// pub async fn my_service(port: Arc<i32>, db: Arc<String>) -> anyhow::Result<()> {
///     // service implementation
/// }
///
/// // Intentionally synchronous (fast, no I/O):
/// #[service]
/// #[allow(sync_handler)]
/// pub fn my_sync_service() -> anyhow::Result<()> {
///     Ok(())
/// }
/// ```
///
/// Then in main.rs:
/// ```rust,ignore
/// let mut daemon = ServiceDaemon::builder().build();
/// daemon.run().await;
/// daemon.wait().await?;
/// ```
#[proc_macro_attribute]
#[proc_macro_error]
pub fn service(attr: TokenStream, item: TokenStream) -> TokenStream {
    service::service_impl(attr, item)
}

/// Marks a struct or function as a type-based dependency provider.
///
/// The macro automatically implements `Provided` for the struct, enabling
/// compile-time verified dependency injection.
///
/// # Example with default value
/// ```rust,ignore
/// use service_daemon::provider;
///
/// #[provider(8080)]
/// pub struct Port(pub i32);
///
/// #[provider("mysql://localhost")]  // Auto-expands to .to_owned()
/// pub struct DbUrl(pub String);
/// ```
///
/// # Example with environment variable
/// ```rust,ignore
/// use service_daemon::provider;
///
/// // String field: env var used directly
/// #[provider("localhost:5432", env = "DATABASE_HOST")]
/// pub struct DatabaseHost(pub String);
///
/// // Non-String field: env var auto-parsed via `.parse()`
/// #[provider(8080, env = "PORT")]
/// pub struct Port(pub i32);
///
/// // Env-only (no default — panics if env var is missing)
/// #[provider(env = "API_KEY")]
/// pub struct ApiKey(pub String);
/// ```
///
/// # Example with template
/// ```rust,ignore
/// use service_daemon::provider;
///
/// #[provider(Queue(String))]
/// pub struct TaskQueue;
///
/// #[provider(Notify)]
/// pub struct MySignal;
/// ```
///
/// # Example with dependencies
/// ```rust,ignore
/// use service_daemon::provider;
/// use std::sync::Arc;
///
/// // Non-Arc fields must implement `Default`.
/// #[provider]
/// pub struct AppConfig {
///     pub port: Arc<Port>,
///     pub db_url: Arc<DbUrl>,
/// }
/// ```
///
/// # Example with async function (dependency injection)
/// ```rust,ignore
/// use service_daemon::provider;
/// use std::sync::Arc;
///
/// #[provider]
/// pub async fn db_pool(url: Arc<DbUrl>) -> DatabasePool {
///     DatabasePool::connect(&url).await.expect("DB connection failed")
/// }
/// ```
#[proc_macro_attribute]
#[proc_macro_error]
pub fn provider(attr: TokenStream, item: TokenStream) -> TokenStream {
    provider::provider_impl(attr, item)
}

/// Marks a function as an event-driven trigger.
///
/// The macro automatically registers the trigger in the global registry
/// using `linkme`. The trigger will be started by `ServiceDaemon` and will
/// execute the function when the specified event occurs.
///
/// > [!NOTE]
/// > Triggers are specialized services. Like normal services, they should
/// > be asynchronous to avoid blocking the event loop host.
///
/// # Syntax
/// `#[trigger(Template(Target))]` or `#[trigger(Template(Target), priority = N)]`
///
/// where `Template` is a valid trigger template variant and `Target` is the
/// provider type that supplies the event source.
///
/// # Template Types
/// - `Cron`: Uses cron expressions. Target should be a provider for `String` (the cron expression).
/// - `Queue`/`BQueue`: Broadcast queue (fanout). Target should be a `#[provider(Queue(T))]`.
/// - `Event`/`Notify`/`Custom`: Signal trigger. Target should be a `#[provider(Notify)]`.
/// - `Watch`/`State`: State change trigger. Fires when the target provider is modified.
///
/// # Example
/// ```rust,ignore
/// use service_daemon::trigger;
///
/// #[trigger(Event(MyNotifier))]
/// async fn on_event() -> anyhow::Result<()> {
///     println!("Event triggered!");
///     Ok(())
/// }
///
/// #[trigger(Queue(TaskQueue))]
/// async fn on_queue_item(item: String) -> anyhow::Result<()> {
///     println!("Received queue item: {}", item);
///     Ok(())
/// }
///
/// #[trigger(Cron(CleanupSchedule))]
/// async fn on_cron_tick() -> anyhow::Result<()> {
///     println!("Cron triggered!");
///     Ok(())
/// }
///
/// #[trigger(Watch(MetricsData), priority = 80)]
/// async fn on_metrics_changed(snapshot: Arc<MetricsData>) -> anyhow::Result<()> {
///     println!("Metrics changed!");
///     Ok(())
/// }
/// ```
#[proc_macro_attribute]
#[proc_macro_error]
pub fn trigger(attr: TokenStream, item: TokenStream) -> TokenStream {
    trigger::trigger_impl(attr, item)
}
