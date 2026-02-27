//! Built-in trigger host implementations.
//!
//! Each host implements [`TriggerHost<T>`] with a two-phase lifecycle:
//!
//! 1. **`setup`**: One-time initialisation -- acquire receivers, register cron
//!    jobs, etc. Resources are stored as struct fields.
//! 2. **`handle_step`**: Per-iteration logic -- wait for the next event using
//!    the resources initialised in `setup`.
//!
//! This eliminates the `shelve` / `shelve_clone` pattern that previously
//! caused deep nesting inside `handle_step`.
//!
//! # Adding a new trigger host
//!
//! 1. Define a struct with the resources it needs.
//! 2. Implement `setup` to initialise those resources.
//! 3. Implement `handle_step` using `&mut self` to access them.
//! 4. Done! The engine takes care of the rest.

use futures::future::BoxFuture;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::core::context;
use crate::core::di::Provided;
use crate::models::trigger::{TriggerHost, TriggerTransition};

// ===========================================================================
// SignalHost -- Signal (Notify) Trigger Host
// ===========================================================================

/// Signal-based trigger host.
///
/// Listens on a `tokio::sync::Notify` and fires the handler each time the
/// notify is triggered. Ideal for lightweight, payload-free events.
///
/// # Aliases
/// `TT::Notify`, `TT::Event`, `TT::Signal`, `TT::Custom`.
pub struct SignalHost;

impl<T> TriggerHost<T> for SignalHost
where
    T: Provided + std::ops::Deref<Target = tokio::sync::Notify> + Send + Sync + 'static,
{
    type Payload = ();

    fn setup(_target: Arc<T>) -> BoxFuture<'static, anyhow::Result<Self>> {
        Box::pin(async { Ok(SignalHost) })
    }

    fn handle_step<'a>(
        &'a mut self,
        target: &'a Arc<T>,
    ) -> BoxFuture<'a, TriggerTransition<Self::Payload>> {
        Box::pin(async move {
            target.notified().await;
            TriggerTransition::Next(())
        })
    }
}

/// Broadcast queue trigger host (fan-out).
///
/// Subscribes to a `tokio::sync::broadcast` channel and delivers every
/// received message to the handler. All subscribers see all messages.
///
/// **Note:** Because the payload type `P` is determined by the `TriggerHost<T>`
/// impl (not at the struct level), the broadcast receiver cannot be stored as
/// a struct field. Instead, it is created during `setup` via `shelve` for
/// cross-iteration persistence. This is a pragmatic exception to the stateful
/// host pattern, necessary for backward-compatible type aliases (`TT::Queue`).
///
/// # Aliases
/// `TT::Queue`, `TT::BQueue`, `TT::BroadcastQueue`.
pub struct TopicHost;

/// Shelve key for the broadcast receiver bridge (TopicHost only).
const TOPIC_BRIDGE_KEY: &str = "__topic_bridge_rx";

impl<T, P> TriggerHost<T> for TopicHost
where
    T: Provided
        + std::ops::Deref<Target = tokio::sync::broadcast::Sender<P>>
        + Send
        + Sync
        + 'static,
    P: Clone + Send + Sync + 'static,
{
    type Payload = P;

    fn setup(target: Arc<T>) -> BoxFuture<'static, anyhow::Result<Self>> {
        Box::pin(async move {
            // Subscribe and shelve the receiver for handle_step iterations.
            // This is a targeted use of shelve -- the receiver type is only known
            // at this impl level, not at the struct level.
            let rx = Arc::new(Mutex::new(target.subscribe()));
            context::shelve(TOPIC_BRIDGE_KEY, rx).await;
            Ok(TopicHost)
        })
    }

    fn handle_step<'a>(
        &'a mut self,
        _target: &'a Arc<T>,
    ) -> BoxFuture<'a, TriggerTransition<Self::Payload>> {
        Box::pin(async move {
            // The receiver was created and shelved during setup.
            let rx_bridge: Arc<Mutex<tokio::sync::broadcast::Receiver<P>>> =
                context::shelve_clone(TOPIC_BRIDGE_KEY)
                    .await
                    .expect("TopicHost receiver must be initialized in setup");

            let result = rx_bridge.lock().await.recv().await;
            match result {
                Ok(value) => TriggerTransition::Next(value),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("Queue trigger lagged by {} messages", n);
                    TriggerTransition::Stop
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    warn!("Queue trigger channel closed");
                    TriggerTransition::Stop
                }
            }
        })
    }
}

// ===========================================================================
// CronHost -- Cron Trigger Host
// ===========================================================================

/// Cron-based scheduled trigger host.
///
/// Registers a job with the shared `tokio-cron-scheduler` and fires the
/// handler on each cron tick.
///
/// All initialisation (scheduler acquisition, job creation, job registration)
/// happens in `setup`. The `handle_step` body is a single `notified().await`.
///
/// # Aliases
/// `TT::Cron`.
#[cfg(feature = "cron")]
pub struct CronHost {
    /// Bridge between the cron callback and our event loop.
    bridge: Arc<tokio::sync::Notify>,
}

#[cfg(feature = "cron")]
static SHARED_SCHEDULER: tokio::sync::OnceCell<tokio_cron_scheduler::JobScheduler> =
    tokio::sync::OnceCell::const_new();

#[cfg(feature = "cron")]
async fn get_shared_scheduler() -> anyhow::Result<tokio_cron_scheduler::JobScheduler> {
    SHARED_SCHEDULER
        .get_or_try_init(|| async {
            let sched = tokio_cron_scheduler::JobScheduler::new().await?;
            sched.start().await?;
            Ok::<_, anyhow::Error>(sched)
        })
        .await
        .cloned()
}

#[cfg(feature = "cron")]
impl<T> TriggerHost<T> for CronHost
where
    T: Provided + std::ops::Deref<Target = String> + Send + Sync + 'static,
{
    type Payload = ();

    fn setup(target: Arc<T>) -> BoxFuture<'static, anyhow::Result<Self>> {
        Box::pin(async move {
            let notify = Arc::new(tokio::sync::Notify::new());
            let notify_for_job = notify.clone();
            let schedule = target.to_string();

            let sched = get_shared_scheduler().await?;

            let job = tokio_cron_scheduler::Job::new_async(&schedule, move |_uuid, _lock| {
                let n = notify_for_job.clone();
                Box::pin(async move {
                    n.notify_waiters();
                })
            })?;

            sched.add(job).await?;
            info!("Registered cron job with schedule '{}'", schedule);

            Ok(CronHost { bridge: notify })
        })
    }

    fn handle_step<'a>(
        &'a mut self,
        _target: &'a Arc<T>,
    ) -> BoxFuture<'a, TriggerTransition<Self::Payload>> {
        Box::pin(async move {
            self.bridge.notified().await;
            TriggerTransition::Next(())
        })
    }
}

// ===========================================================================
// WatchHost -- State Change Trigger Host
// ===========================================================================

/// State-watch trigger host.
///
/// Fires once with the current state snapshot, then idles via
/// `TriggerTransition::Reload`. The framework's `ServiceWatcher` will
/// restart us when the target provider changes.
///
/// # Aliases
/// `TT::Watch`, `TT::State`.
pub struct WatchHost;

impl<T> TriggerHost<T> for WatchHost
where
    T: Provided + Send + Sync + 'static,
{
    type Payload = ();

    fn setup(_target: Arc<T>) -> BoxFuture<'static, anyhow::Result<Self>> {
        Box::pin(async { Ok(WatchHost) })
    }

    fn handle_step<'a>(
        &'a mut self,
        _target: &'a Arc<T>,
    ) -> BoxFuture<'a, TriggerTransition<Self::Payload>> {
        Box::pin(async {
            // Fire once, then tell the engine to idle until reload.
            TriggerTransition::Reload(())
        })
    }
}
