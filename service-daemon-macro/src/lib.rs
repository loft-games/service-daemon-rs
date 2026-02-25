//! Service Daemon Macro Crate
//!
//! This crate provides procedural macros for the `service-daemon` library:
//! - `#[service]` - Mark functions as managed services
//! - `#[trigger]` - Event-driven trigger functions
//! - `#[provider]` - Dependency injection providers
//! - `#[allow_sync]` - Suppress warnings for intentionally synchronous functions

use proc_macro::TokenStream;
use proc_macro_error2::proc_macro_error;

mod allow_sync;
mod common;
mod provider;
mod service;
mod trigger;

// Internal Module Structure:
// - trigger/: Macro logic for #[trigger]. Split into mod.rs (main), parser.rs (attributes), and codegen.rs (logic).
// - service/: Macro logic for #[service]. Split into mod.rs (main) and codegen.rs (helpers).
// - provider/: Macro logic for #[provider]. Split into mod.rs, parser.rs, templates.rs (special types), and struct_gen.rs (DI).

/// Marks a synchronous function as intentionally not needing `async`.
///
/// Use this attribute to suppress warnings about synchronous functions
/// blocking the async executor. Only use this when you are certain that
/// the synchronous function will not perform blocking I/O or long-running
/// computations.
///
/// > [!CAUTION]
/// > **Misuse Warning**: Using this on truly blocking code (e.g., `std::thread::sleep`,
/// > network I/O) will stall the entire daemon and break graceful shutdown.
/// > For blocking operations, use `tokio::task::spawn_blocking` instead.
///
/// # Example
/// ```rust,ignore
/// use service_daemon::{service, allow_sync};
///
/// #[allow_sync]
/// #[service]
/// pub fn my_fast_sync_service() -> anyhow::Result<()> {
///     // This function is intentionally sync and won't block.
///     Ok(())
/// }
/// ```
#[proc_macro_attribute]
pub fn allow_sync(attr: TokenStream, item: TokenStream) -> TokenStream {
    allow_sync::allow_sync_impl(attr, item)
}

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
/// > functions will trigger a runtime warning unless annotated with `#[allow_sync]`.
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
/// ```
///
/// Then in main.rs:
/// ```rust,ignore
/// let daemon = ServiceDaemon::builder().build();
/// daemon.run().await?;
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
/// #[provider(default = 8080)]
/// pub struct Port(pub i32);
///
/// #[provider(default = "mysql://localhost")]  // Auto-expands to .to_owned()
/// pub struct DbUrl(pub String);
/// ```
///
/// # Example with environment variable
/// ```rust,ignore
/// use service_daemon::provider;
///
/// #[provider(default = "localhost:5432", env_name = "DATABASE_HOST")]
/// pub struct DatabaseHost(pub String);
/// ```
///
/// # Example with dependencies
/// ```rust,ignore
/// use service_daemon::provider;
/// use std::sync::Arc;
///
/// #[provider]
/// pub struct AppConfig {
///     pub port: Arc<Port>,
///     pub db_url: Arc<DbUrl>,
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
/// - `Queue`/`BQueue`: Broadcast queue (fanout). Target should be a `#[provider(default = Queue)]`.
/// - `LBQueue`: Load-balancing queue. Target should be a `#[provider(default = LBQueue)]`.
/// - `Event`/`Notify`/`Custom`: Signal trigger. Target should be a `#[provider(default = Notify)]`.
/// - `Watch`/`State`: State change trigger. Fires when the target provider is modified.
///
/// # Example
/// ```rust,ignore
/// use service_daemon::trigger;
///
/// #[trigger(Event(MyNotifier))]
/// async fn on_event(payload: (), trigger_id: String) -> anyhow::Result<()> {
///     println!("Event triggered! ID: {}", trigger_id);
///     Ok(())
/// }
///
/// #[trigger(Queue(TaskQueue))]
/// async fn on_queue_item(item: String, trigger_id: String) -> anyhow::Result<()> {
///     println!("Received queue item: {} (trigger: {})", item, trigger_id);
///     Ok(())
/// }
///
/// #[trigger(Cron(CleanupSchedule))]
/// async fn on_cron_tick(tick_time: (), trigger_id: String) -> anyhow::Result<()> {
///     println!("Cron triggered! ID: {}", trigger_id);
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
