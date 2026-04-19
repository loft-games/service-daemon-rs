//! Built-in trigger host implementations.
//!
//! Each host implements [`TriggerHost<T>`] with a two-phase lifecycle:
//!
//! 1. **`setup`**: One-time initialisation -- acquire receivers, register cron
//!    jobs, etc. Resources are stored as struct fields.
//! 2. **`handle_step`**: Per-iteration logic -- wait for the next event using
//!    the resources initialised in `setup`.
//!
//! # Adding a new trigger host
//!
//! 1. Define a struct with the resources it needs.
//! 2. Implement `setup` to initialise those resources.
//! 3. Implement `handle_step` using `&mut self` to access them.
//! 4. Done! The engine takes care of the rest.

#[cfg(feature = "cron")]
use anyhow::Error;
use anyhow::Result;
use futures::future::BoxFuture;
use std::any::{Any, TypeId};
use std::ops::Deref;
use std::sync::Arc;
#[cfg(feature = "cron")]
use tokio::sync::Notify;
use tokio::sync::{Mutex, broadcast};
#[cfg(feature = "cron")]
use tracing::error;
use tracing::{info, warn};

use crate::core::di::{Provided, WatchableProvided};
use crate::core::managed_state::{TrackedNotify, TrackedSender};
#[cfg(feature = "cron")]
use crate::models::ServiceError;
use crate::models::policy::ScalingPolicy;
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
    T: Provided + Deref<Target = TrackedNotify> + Send + Sync + 'static,
{
    type Payload = ();

    fn setup(_target: Arc<T>) -> BoxFuture<'static, Result<Self>> {
        Box::pin(async { Ok(SignalHost) })
    }

    fn handle_step<'a>(
        &'a mut self,
        target: &'a Arc<T>,
    ) -> BoxFuture<'a, TriggerTransition<Self::Payload>> {
        Box::pin(async move {
            target.notified().await;
            // Read the pre-generated (message_id, source_id) from notify_waiters()
            let identity = target.last_id();
            TriggerTransition::Next((), identity)
        })
    }
}

/// Broadcast queue trigger host (fan-out).
///
/// Subscribes to a `tokio::sync::broadcast` channel and delivers every
/// received message to the handler. All subscribers see all messages.
///
/// The broadcast receiver is stored as a type-erased field (`Box<dyn Any>`)
/// because the payload type `P` is determined by the `TriggerHost<T>` impl
/// (not at the struct level). This avoids the previous `shelve` / magic-string
/// pattern while remaining compatible with the macro's unparameterized
/// `TT::Queue` alias.
///
/// # Aliases
/// `TT::Queue`, `TT::BQueue`, `TT::BroadcastQueue`.
pub struct TopicHost {
    /// Type-erased broadcast receiver bridge, initialized in `setup()`.
    /// Concrete type: `Arc<Mutex<broadcast::Receiver<P>>>`.
    receiver: Box<dyn Any + Send + Sync>,
    /// `TypeId` of the concrete receiver captured at `setup()` time.
    /// Cross-checked against the per-call `P` in `handle_step()` so a macro
    /// mismatch fails with an actionable diagnostic rather than a bare
    /// downcast panic.
    receiver_type: TypeId,
}

impl<T, P> TriggerHost<T> for TopicHost
where
    T: Provided + Deref<Target = TrackedSender<P>> + Send + Sync + 'static,
    P: Clone + Send + Sync + 'static,
{
    type Payload = P;

    fn setup(target: Arc<T>) -> BoxFuture<'static, Result<Self>> {
        Box::pin(async move {
            let rx: Arc<Mutex<broadcast::Receiver<P>>> = Arc::new(Mutex::new(target.subscribe()));
            Ok(TopicHost {
                receiver: Box::new(rx),
                receiver_type: TypeId::of::<Arc<Mutex<broadcast::Receiver<P>>>>(),
            })
        })
    }

    fn handle_step<'a>(
        &'a mut self,
        target: &'a Arc<T>,
    ) -> BoxFuture<'a, TriggerTransition<Self::Payload>> {
        Box::pin(async move {
            // Cross-check the setup-time `P` against the call-time `P`.
            // These must match because both are driven by the macro's generic
            // param; a mismatch would signal a framework bug (not user error).
            let expected = TypeId::of::<Arc<Mutex<broadcast::Receiver<P>>>>();
            assert_eq!(
                self.receiver_type, expected,
                "TopicHost payload type mismatch (setup `P` differs from handle_step `P`); \
                 this indicates a macro-generated invariant violation in service-daemon-macro"
            );

            // Recover the concrete receiver type from the type-erased field.
            let rx_bridge: &Arc<Mutex<broadcast::Receiver<P>>> = self
                .receiver
                .downcast_ref()
                .expect("TopicHost receiver type mismatch (internal bug)");

            let result = rx_bridge.lock().await.recv().await;
            match result {
                Ok(value) => {
                    // Read the pre-generated (message_id, source_id) from the TrackedSender
                    let identity = target.last_id();
                    TriggerTransition::Next(value, identity)
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("Queue trigger lagged by {} messages", n);
                    TriggerTransition::Stop
                }
                Err(broadcast::error::RecvError::Closed) => {
                    warn!("Queue trigger channel closed");
                    TriggerTransition::Stop
                }
            }
        })
    }

    /// TopicHost is a streaming event source -- declare elastic scaling.
    fn scaling_policy() -> Option<ScalingPolicy> {
        Some(ScalingPolicy::default())
    }
}

// ===========================================================================
// CronHost -- Cron Trigger Host
// ===========================================================================

#[cfg(feature = "cron")]
use tokio::sync::OnceCell;
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
use tokio_cron_scheduler::{Job, JobScheduler};

#[cfg(feature = "cron")]
pub struct CronHost {
    /// Bridge between the cron callback and our event loop.
    bridge: Arc<Notify>,
}

/// Process-wide cron scheduler shared by all trigger hosts.
///
/// The daemon owns the process, so a single `OnceCell` scheduler is fine.
/// Multiple `ServiceDaemon` instances (rare) will share this scheduler.
#[cfg(feature = "cron")]
static SHARED_SCHEDULER: OnceCell<JobScheduler> = OnceCell::const_new();

#[cfg(feature = "cron")]
async fn get_shared_scheduler() -> Result<JobScheduler> {
    SHARED_SCHEDULER
        .get_or_try_init(|| async {
            let sched = JobScheduler::new().await?;
            sched.start().await?;
            Ok::<_, Error>(sched)
        })
        .await
        .cloned()
}

#[cfg(feature = "cron")]
impl<T> TriggerHost<T> for CronHost
where
    T: Provided + Deref<Target = String> + Send + Sync + 'static,
{
    type Payload = ();

    fn setup(target: Arc<T>) -> BoxFuture<'static, Result<Self>> {
        Box::pin(async move {
            let notify = Arc::new(Notify::new());
            let notify_for_job = notify.clone();
            let schedule = target.to_string();

            // Any failure from here on is a configuration error (invalid cron
            // expression, scheduler init failure). Surface it as a fatal
            // service error so the supervisor stops retrying instead of
            // silently masking a broken trigger.
            fn to_fatal(schedule: &str, source: Error) -> Error {
                error!(
                    schedule = %schedule,
                    error = %source,
                    "CronHost setup failed; trigger will not fire. Check cron expression and scheduler state."
                );
                anyhow::Error::new(ServiceError::Fatal(format!(
                    "CronHost setup failed for schedule '{schedule}': {source}"
                )))
            }

            let sched = get_shared_scheduler()
                .await
                .map_err(|e| to_fatal(&schedule, e))?;

            let job = Job::new_async(&schedule, move |_uuid, _lock| {
                let n = notify_for_job.clone();
                Box::pin(async move {
                    n.notify_waiters();
                })
            })
            .map_err(|e| to_fatal(&schedule, anyhow::Error::new(e)))?;

            sched
                .add(job)
                .await
                .map_err(|e| to_fatal(&schedule, anyhow::Error::new(e)))?;
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
            TriggerTransition::Next((), None)
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
    T: WatchableProvided + Send + Sync + 'static,
{
    type Payload = ();

    fn setup(_target: Arc<T>) -> BoxFuture<'static, Result<Self>> {
        Box::pin(async { Ok(WatchHost) })
    }

    fn handle_step<'a>(
        &'a mut self,
        _target: &'a Arc<T>,
    ) -> BoxFuture<'a, TriggerTransition<Self::Payload>> {
        Box::pin(async {
            // Fire once, then tell the engine to idle until reload.
            TriggerTransition::Reload((), None)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `TopicHost` the same way `<TopicHost as TriggerHost<T>>::setup`
    /// does internally, bypassing the `Provided` trait bound that requires a
    /// full daemon context. This mirrors the production body of `setup()`.
    fn make_topic_host_for<P>(sender: &TrackedSender<P>) -> TopicHost
    where
        P: Clone + Send + Sync + 'static,
    {
        let rx: Arc<Mutex<broadcast::Receiver<P>>> = Arc::new(Mutex::new(sender.subscribe()));
        TopicHost {
            receiver: Box::new(rx),
            receiver_type: TypeId::of::<Arc<Mutex<broadcast::Receiver<P>>>>(),
        }
    }

    /// `TopicHost` must capture a concrete `TypeId` at setup time so that a
    /// macro-generated mismatch between the `P` passed to `setup()` and the
    /// `P` used in `handle_step()` fails with a named assertion rather than a
    /// bare downcast panic. This test pins the invariant for two payload
    /// types and verifies the TypeIds are distinct.
    #[test]
    fn topic_host_captures_concrete_receiver_type() {
        let sender_i32 = TrackedSender::<i32>::new(4);
        let host_i32 = make_topic_host_for(&sender_i32);
        assert_eq!(
            host_i32.receiver_type,
            TypeId::of::<Arc<Mutex<broadcast::Receiver<i32>>>>(),
        );

        let sender_string = TrackedSender::<String>::new(4);
        let host_string = make_topic_host_for(&sender_string);
        assert_eq!(
            host_string.receiver_type,
            TypeId::of::<Arc<Mutex<broadcast::Receiver<String>>>>(),
        );

        assert_ne!(host_i32.receiver_type, host_string.receiver_type);
    }
}
