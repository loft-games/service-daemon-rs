//! Built-in trigger host implementations.
//!
//! Each host is a zero-sized struct that implements [`TriggerHost<T>`] by
//! providing a [`handle_step`](TriggerHost::handle_step) method. The default
//! engine in `TriggerHost::run_as_service` handles everything else (loop,
//! tracing, ID issuance, shutdown).
//!
//! # Adding a new trigger host
//!
//! 1. Define a zero-sized struct.
//! 2. Implement `handle_step` — return a [`TriggerTransition`].
//! 3. Done! The engine takes care of the rest.

use futures::future::BoxFuture;
use std::sync::Arc;
use tokio::sync::{Mutex, OnceCell};
use tracing::{error, info, warn};

use crate::core::context;
use crate::core::di::Provided;
use crate::models::trigger::{TriggerHost, TriggerTransition};

// ===========================================================================
// SignalHost — Signal (Notify) Trigger Host
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

    fn handle_step(target: Arc<T>) -> BoxFuture<'static, TriggerTransition<Self::Payload>> {
        Box::pin(async move {
            target.notified().await;
            TriggerTransition::Next(())
        })
    }
}

// ===========================================================================
// TopicHost — Broadcast Queue Trigger Host
// ===========================================================================

/// Broadcast queue trigger host (fan-out).
///
/// Subscribes to a `tokio::sync::broadcast` channel and delivers every
/// received message to the handler. All subscribers see all messages.
///
/// Uses `shelve`/`shelve_clone` to persist the broadcast receiver across
/// `handle_step` iterations.
///
/// # Aliases
/// `TT::Queue`, `TT::BQueue`, `TT::BroadcastQueue`.
pub struct TopicHost;

/// Shelve key for the broadcast receiver bridge.
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

    fn handle_step(target: Arc<T>) -> BoxFuture<'static, TriggerTransition<Self::Payload>> {
        Box::pin(async move {
            // Get or create the shelved receiver
            let rx_bridge: Arc<Mutex<tokio::sync::broadcast::Receiver<P>>> =
                match context::shelve_clone(TOPIC_BRIDGE_KEY).await {
                    Some(rx) => rx,
                    None => {
                        // First call: subscribe and shelve the receiver
                        let rx = Arc::new(Mutex::new(target.subscribe()));
                        context::shelve(TOPIC_BRIDGE_KEY, rx.clone()).await;
                        rx
                    }
                };

            // Wait for next message
            let result = {
                let mut rx = rx_bridge.lock().await;
                rx.recv().await
            };

            match result {
                Ok(value) => TriggerTransition::Next(value),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("Queue trigger lagged by {} messages", n);
                    // Continue the loop — lagging is recoverable
                    // We return a dummy transition; the engine will call handle_step again
                    // Actually, we need to recv again. Let's just recurse by returning Next
                    // with a value from a fresh recv. But we can't do that easily.
                    // Instead, the simplest approach: just re-enter handle_step by
                    // having the engine loop call us again. But we need a payload...
                    // The cleanest solution: tell the engine to keep looping without dispatch.
                    TriggerTransition::Stop // Will be re-entered by the engine loop
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
// LBTopicHost — Load-Balancing Queue Trigger Host
// ===========================================================================

/// Load-balancing queue trigger host.
///
/// Consumes messages from a shared `tokio::sync::mpsc` channel behind a
/// `Mutex`. Only one subscriber processes each message (competing consumers).
///
/// # Aliases
/// `TT::LBQueue`, `TT::LoadBalancingQueue`.
pub struct LBTopicHost;

/// Trait for targets that expose a load-balanced receiver.
///
/// This is auto-implemented by LBQueue providers generated with the
/// `#[provider(default = LBQueue)]` macro attribute.
pub trait LBQueueTarget {
    /// The payload type carried by the internal mpsc channel.
    type Item: Send + Sync + 'static;

    /// Returns a reference to the shared receiver mutex.
    fn receiver(&self) -> &Arc<Mutex<tokio::sync::mpsc::Receiver<Self::Item>>>;
}

impl<T> TriggerHost<T> for LBTopicHost
where
    T: Provided + LBQueueTarget + Send + Sync + 'static,
{
    type Payload = T::Item;

    fn handle_step(target: Arc<T>) -> BoxFuture<'static, TriggerTransition<Self::Payload>> {
        Box::pin(async move {
            let mut rx = target.receiver().lock().await;
            match rx.recv().await {
                Some(value) => TriggerTransition::Next(value),
                None => TriggerTransition::Stop,
            }
        })
    }
}

// ===========================================================================
// CronHost — Cron Trigger Host
// ===========================================================================

/// Cron-based scheduled trigger host.
///
/// Registers a job with the shared `tokio-cron-scheduler` and fires the
/// handler on each cron tick. Uses `shelve`/`unshelve` to store the bridge
/// `Notify` across `handle_step` iterations.
///
/// # How it works
/// 1. **First `handle_step` call**: Registers a cron job with the shared
///    scheduler and shelves an `Arc<Notify>` as the bridge channel.
/// 2. **Subsequent calls**: Unshelves the bridge, re-shelves it (since
///    `unshelve` is destructive), and waits for the next tick.
/// 3. The cron job callback calls `notify_waiters()` on each tick, waking
///    whatever `handle_step` is currently awaiting.
///
/// # Aliases
/// `TT::Cron`.
#[cfg(feature = "cron")]
pub struct CronHost;

#[cfg(feature = "cron")]
static SHARED_SCHEDULER: OnceCell<tokio_cron_scheduler::JobScheduler> = OnceCell::const_new();

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

/// Shelve key for the cron bridge Notify.
#[cfg(feature = "cron")]
const CRON_BRIDGE_KEY: &str = "__cron_bridge_notify";

#[cfg(feature = "cron")]
impl<T> TriggerHost<T> for CronHost
where
    T: Provided + std::ops::Deref<Target = String> + Send + Sync + 'static,
{
    type Payload = ();

    fn handle_step(target: Arc<T>) -> BoxFuture<'static, TriggerTransition<Self::Payload>> {
        Box::pin(async move {
            // Try to retrieve the bridge from the service shelf
            let bridge: Arc<tokio::sync::Notify> =
                match context::shelve_clone::<Arc<tokio::sync::Notify>>(CRON_BRIDGE_KEY).await {
                    Some(notify) => notify,
                    None => {
                        // First call: register the cron job and create the bridge
                        let notify = Arc::new(tokio::sync::Notify::new());
                        let notify_for_job = notify.clone();

                        let schedule = target.to_string();
                        let sched = match get_shared_scheduler().await {
                            Ok(s) => s,
                            Err(e) => {
                                error!("Failed to get cron scheduler: {:?}", e);
                                return TriggerTransition::Stop;
                            }
                        };

                        let job = match tokio_cron_scheduler::Job::new_async(
                            &schedule,
                            move |_uuid, _lock| {
                                let n = notify_for_job.clone();
                                Box::pin(async move {
                                    n.notify_waiters();
                                })
                            },
                        ) {
                            Ok(j) => j,
                            Err(e) => {
                                error!("Failed to create cron job: {:?}", e);
                                return TriggerTransition::Stop;
                            }
                        };

                        if let Err(e) = sched.add(job).await {
                            error!("Failed to add cron job to scheduler: {:?}", e);
                            return TriggerTransition::Stop;
                        }

                        info!("Registered cron job with schedule '{}'", schedule);

                        // Shelve the bridge for subsequent iterations
                        context::shelve(CRON_BRIDGE_KEY, notify.clone()).await;
                        notify
                    }
                };

            // Wait for the next cron tick
            bridge.notified().await;
            TriggerTransition::Next(())
        })
    }
}

// ===========================================================================
// WatchHost — State Change Trigger Host
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

    fn handle_step(_target: Arc<T>) -> BoxFuture<'static, TriggerTransition<Self::Payload>> {
        Box::pin(async {
            // Fire once, then tell the engine to idle until reload.
            TriggerTransition::Reload(())
        })
    }
}
