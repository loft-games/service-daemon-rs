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
/// let daemon = ServiceDaemon::from_registry();
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
/// # Attributes
/// - `template`: The trigger template type. Options: `cron`, `queue`, `lb_queue`, `event`, `notify`, `custom`.
/// - `target`: The provider type (struct) that supplies the event source.
///
/// # Template Types
/// - `cron`: Uses cron expressions. Target should be a provider for `String` (the cron expression).
/// - `queue`: Broadcast queue (fanout). Target should be a `#[provider(default = Queue)]`.
/// - `lb_queue`: Load-balancing queue. Target should be a `#[provider(default = LBQueue)]`.
/// - `event` / `notify` / `custom`: Signal trigger. Target should be a `#[provider(default = Notify)]`.
///
/// # Example
/// ```rust,ignore
/// use service_daemon::trigger;
///
/// #[trigger(template = event, target = MyNotifier)]
/// async fn on_event(payload: (), trigger_id: String) -> anyhow::Result<()> {
///     println!("Event triggered! ID: {}", trigger_id);
///     Ok(())
/// }
///
/// #[trigger(template = queue, target = TaskQueue)]
/// async fn on_queue_item(item: String, trigger_id: String) -> anyhow::Result<()> {
///     println!("Received queue item: {} (trigger: {})", item, trigger_id);
///     Ok(())
/// }
///
/// #[trigger(template = cron, target = CleanupSchedule)]
/// async fn on_cron_tick(tick_time: (), trigger_id: String) -> anyhow::Result<()> {
///     println!("Cron triggered! ID: {}", trigger_id);
///     Ok(())
/// }
/// ```
#[proc_macro_attribute]
#[proc_macro_error]
pub fn trigger(attr: TokenStream, item: TokenStream) -> TokenStream {
    trigger::trigger_impl(attr, item)
}
