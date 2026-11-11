//! Trigger interceptor infrastructure.
//!
//! This module provides [`TriggerInterceptor`], a composable middleware trait
//! that gives each interceptor full control over the dispatch lifecycle. Combined
//! with [`TriggerRunner`], it encapsulates the event loop, signal handling,
//! interceptor pipeline, tracing, and retry logic.
//!
//! # Architecture (Onion Model)
//!
//! ```text
//!   handle_step --> TriggerRunner.run_with_host()
//!                       |
//!                       v
//!                   dispatch(payload)
//!                       +-- TracingInterceptor.intercept(ctx, next)
//!                             +-- RetryInterceptor.intercept(ctx, next)
//!                                   +-- handler(TriggerContext)
//! ```
//!
//! Each interceptor receives a [`DispatchContext`] by value and a `next`
//! callback. The interceptor decides **if, when, and how many times** to
//! call `next`, enabling framework-owned patterns like retry and tracing spans.
//!
//! The `TriggerRunner` owns the `select!` + shutdown logic, so trigger hosts
//! only need to implement `handle_step`.

mod dispatch;
mod drain;
mod event_loop;
mod failure;
mod interceptors;
mod message_id;
mod scaling;

use dispatch::TriggerInterceptor;
pub(crate) use failure::{TriggerDispatchFailure, TriggerDispatchFailureKind};

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize};

use tokio::sync::Semaphore;

use crate::core::context;
use crate::core::runtime_facts::TriggerRuntimeFactsHandle;
use crate::core::trigger_policy_overlay::{TriggerBasePolicy, TriggerPolicyOverlayStore};
use crate::models::policy::{RestartPolicy, ScalingPolicy};
use crate::models::service::ServiceInstanceId;
use crate::models::trigger::TriggerHandler;

use self::interceptors::{RetryInterceptor, TracingInterceptor};

pub struct TriggerRunner<P: Send + Sync + 'static> {
    /// Human-readable name of this trigger service.
    name: &'static str,
    /// The `ServiceInstanceId` of the trigger service.
    service_instance_id: ServiceInstanceId,
    /// Monotonically increasing instance counter for tracing.
    instance_counter: AtomicU64,
    /// The user's event handler.
    handler: TriggerHandler<P>,
    /// Registered interceptor chain (executed in registration order, onion model).
    /// Stored as `Arc` to allow cheap cloning into `tokio::spawn` tasks.
    interceptors: Vec<Arc<dyn TriggerInterceptor<P>>>,
    /// Optional elastic-scaling policy. `None` means serial dispatch
    /// (single permit, no scale monitor).
    scaling: Option<ScalingPolicy>,
    /// Semaphore controlling the number of concurrent handler invocations.
    /// With `scaling = None`, this holds exactly 1 permit (serial mode).
    /// With `scaling = Some(sp)`, starts at `sp.initial_concurrency()`.
    semaphore: Arc<Semaphore>,
    /// Current concurrency limit (tracked separately because `Semaphore`
    /// doesn't expose its total permit count).
    current_limit: Arc<AtomicUsize>,
    /// Read-only runtime facts writer for this trigger, when running inside a daemon scope.
    runtime_facts: Option<TriggerRuntimeFactsHandle>,
    generation: u64,
    base_policy: TriggerBasePolicy,
    policy_overlays: Option<Arc<TriggerPolicyOverlayStore>>,
}

impl<P: Send + Sync + 'static> TriggerRunner<P> {
    /// Create a new runner with the given name, handler, restart policy,
    /// and optional scaling policy.
    ///
    /// The built-in `TracingInterceptor` and `RetryInterceptor` are
    /// automatically registered, providing per-dispatch tracing and
    /// exponential-backoff retry.
    ///
    /// When `scaling` is `Some`, the semaphore is initialized with
    /// `scaling.initial_concurrency()` permits and a background scale
    /// monitor is spawned. When `None`, the semaphore holds exactly
    /// 1 permit (serial dispatch, no elastic scaling).
    ///
    /// The default interceptor order is:
    /// 1. `TracingInterceptor` - wraps everything in a tracing span
    /// 2. `RetryInterceptor` - retries the inner chain on failure
    /// 3. Terminal handler node (implicit)
    pub fn new(
        name: &'static str,
        service_instance_id: ServiceInstanceId,
        handler: TriggerHandler<P>,
        restart_policy: RestartPolicy,
        scaling: Option<ScalingPolicy>,
    ) -> Self {
        let initial = scaling.map_or(1, |sp| sp.initial_concurrency());
        let semaphore = Arc::new(Semaphore::new(initial));
        let current_limit = Arc::new(AtomicUsize::new(initial));
        let generation = context::current_service_generation();
        let base_policy = TriggerBasePolicy {
            restart_policy,
            scaling,
        };
        let runtime_facts = context::register_current_trigger_runtime(
            service_instance_id,
            name,
            generation,
            semaphore.clone(),
            current_limit.clone(),
        );
        let policy_overlays = context::register_current_trigger_policy_overlay(
            service_instance_id,
            generation,
            base_policy,
            semaphore.clone(),
            current_limit.clone(),
        );
        Self {
            name,
            service_instance_id,
            instance_counter: AtomicU64::new(0),
            handler,
            interceptors: vec![Arc::new(TracingInterceptor), Arc::new(RetryInterceptor)],
            scaling,
            semaphore,
            current_limit,
            runtime_facts,
            generation,
            base_policy,
            policy_overlays,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::BoxFuture;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;
    use tokio::sync::Notify;

    use crate::models::trigger::{TriggerHost, TriggerTransition};

    struct OneShotBlockingHost {
        emitted: bool,
    }

    impl TriggerHost<()> for OneShotBlockingHost {
        type Payload = ();

        fn setup(_target: Arc<()>) -> BoxFuture<'static, anyhow::Result<Self>> {
            Box::pin(async { Ok(Self { emitted: false }) })
        }

        fn handle_step<'a>(
            &'a mut self,
            _target: &'a Arc<()>,
        ) -> BoxFuture<'a, TriggerTransition<Self::Payload>> {
            Box::pin(async move {
                if self.emitted {
                    crate::core::context::wait_shutdown().await;
                    TriggerTransition::Stop
                } else {
                    self.emitted = true;
                    TriggerTransition::Next((), None)
                }
            })
        }
    }

    struct DispatchThenStopHost {
        emitted: bool,
        stop_emitted: Arc<Notify>,
    }

    impl TriggerHost<()> for DispatchThenStopHost {
        type Payload = ();

        fn setup(_target: Arc<()>) -> BoxFuture<'static, anyhow::Result<Self>> {
            Box::pin(async {
                Ok(Self {
                    emitted: false,
                    stop_emitted: Arc::new(Notify::new()),
                })
            })
        }

        fn handle_step<'a>(
            &'a mut self,
            _target: &'a Arc<()>,
        ) -> BoxFuture<'a, TriggerTransition<Self::Payload>> {
            Box::pin(async move {
                if self.emitted {
                    self.stop_emitted.notify_one();
                    TriggerTransition::Stop
                } else {
                    self.emitted = true;
                    TriggerTransition::Next((), None)
                }
            })
        }
    }

    struct OneShotReloadHost {
        emitted: bool,
    }

    impl TriggerHost<()> for OneShotReloadHost {
        type Payload = ();

        fn setup(_target: Arc<()>) -> BoxFuture<'static, anyhow::Result<Self>> {
            Box::pin(async { Ok(Self { emitted: false }) })
        }

        fn handle_step<'a>(
            &'a mut self,
            _target: &'a Arc<()>,
        ) -> BoxFuture<'a, TriggerTransition<Self::Payload>> {
            Box::pin(async move {
                if self.emitted {
                    crate::core::context::wait_shutdown().await;
                    TriggerTransition::Stop
                } else {
                    self.emitted = true;
                    TriggerTransition::Reload((), None)
                }
            })
        }
    }

    struct TwoDispatchHost {
        emitted: usize,
        release_second: Arc<Notify>,
    }

    impl TriggerHost<()> for TwoDispatchHost {
        type Payload = ();

        fn setup(_target: Arc<()>) -> BoxFuture<'static, anyhow::Result<Self>> {
            Box::pin(async {
                Ok(Self {
                    emitted: 0,
                    release_second: Arc::new(Notify::new()),
                })
            })
        }

        fn handle_step<'a>(
            &'a mut self,
            _target: &'a Arc<()>,
        ) -> BoxFuture<'a, TriggerTransition<Self::Payload>> {
            Box::pin(async move {
                if self.emitted == 0 {
                    self.emitted += 1;
                    TriggerTransition::Next((), None)
                } else if self.emitted == 1 {
                    self.release_second.notified().await;
                    self.emitted += 1;
                    TriggerTransition::Next((), None)
                } else {
                    TriggerTransition::Stop
                }
            })
        }
    }

    #[tokio::test]
    async fn dispatch_retry_exhaustion_propagates_typed_recoverable_failure() {
        use crate::core::context::{__run_service_scope, DaemonResources, ServiceIdentity};
        use tokio_util::sync::CancellationToken;

        let handler: TriggerHandler<()> =
            Arc::new(|_ctx| Box::pin(async { Err(anyhow::anyhow!("handler failed permanently")) }));
        let restart_policy = RestartPolicy::builder()
            .initial_delay(Duration::from_millis(1))
            .max_delay(Duration::from_millis(1))
            .jitter_factor(0.0)
            .trigger_max_retries(1)
            .build();
        let runner = TriggerRunner::new(
            "failing_dispatch_trigger",
            ServiceInstanceId::new(uuid::Uuid::from_u128(200)),
            handler,
            restart_policy,
            None,
        );

        let result = __run_service_scope(
            ServiceIdentity::new(
                ServiceInstanceId::new(uuid::Uuid::from_u128(200)),
                "failing_dispatch_trigger",
                CancellationToken::new(),
                CancellationToken::new(),
            ),
            DaemonResources::new(),
            || async move {
                let mut host = OneShotBlockingHost { emitted: false };
                let target = Arc::new(());
                tokio::time::timeout(
                    Duration::from_millis(500),
                    runner.run_with_host::<(), OneShotBlockingHost>(&mut host, target),
                )
                .await
                .expect("dispatch failure was not propagated to run_with_host")
            },
        )
        .await;

        let error = result.expect_err("run_with_host should fail when retry is exhausted");
        let failure = error
            .downcast_ref::<TriggerDispatchFailure>()
            .expect("retry exhaustion should use typed trigger dispatch failure");
        assert_eq!(
            failure.kind(),
            TriggerDispatchFailureKind::HandlerRetryExhausted
        );
    }

    #[tokio::test]
    async fn trigger_context_pressure_reads_self_scoped_snapshot() {
        use crate::core::context::{__run_service_scope, DaemonResources, ServiceIdentity};
        use crate::models::TriggerPressureSnapshot;
        use tokio::sync::oneshot;
        use tokio_util::sync::CancellationToken;

        let resources = DaemonResources::new();
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(301));
        let (sender, receiver) = oneshot::channel::<TriggerPressureSnapshot>();
        let sender = Arc::new(StdMutex::new(Some(sender)));
        let handler_sender = sender.clone();
        let handler: TriggerHandler<()> = Arc::new(move |ctx| {
            let handler_sender = handler_sender.clone();
            Box::pin(async move {
                let pressure = ctx
                    .pressure()
                    .expect("trigger pressure should be available in trigger scope");
                if let Some(sender) = handler_sender
                    .lock()
                    .expect("sender mutex should not be poisoned")
                    .take()
                {
                    let _ = sender.send(pressure);
                }
                Ok(())
            })
        });

        let run_result = __run_service_scope(
            ServiceIdentity::new(
                service_instance_id,
                "pressure_trigger",
                CancellationToken::new(),
                CancellationToken::new(),
            ),
            resources.clone(),
            || async move {
                let runner = TriggerRunner::new(
                    "pressure_trigger",
                    service_instance_id,
                    handler,
                    RestartPolicy::default(),
                    None,
                );
                let mut host = DispatchThenStopHost {
                    emitted: false,
                    stop_emitted: Arc::new(Notify::new()),
                };
                runner
                    .run_with_host::<(), DispatchThenStopHost>(&mut host, Arc::new(()))
                    .await
            },
        )
        .await;

        run_result.expect("trigger should complete");
        let observed = receiver
            .await
            .expect("handler should send observed pressure");
        assert_eq!(observed.service_instance_id, service_instance_id);
        assert_eq!(observed.current_limit, 1);
        assert_eq!(observed.in_flight, 1);
        assert_eq!(observed.dispatched_total, 1);

        let snapshot = resources
            .runtime_facts
            .trigger_snapshot(service_instance_id)
            .expect("trigger runtime snapshot should be registered");
        assert_eq!(snapshot.pressure.completed_total, 1);
        assert_eq!(snapshot.pressure.failed_total, 0);
        assert_eq!(snapshot.pressure.in_flight, 0);
    }

    #[tokio::test]
    async fn trigger_policy_overlay_applies_to_future_dispatches() {
        use crate::core::context::{__run_service_scope, DaemonResources, ServiceIdentity};
        use crate::models::{ScalingPolicy, TriggerPolicyOverlay};
        use tokio::sync::oneshot;
        use tokio_util::sync::CancellationToken;

        let resources = DaemonResources::new();
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(303));
        let (sender, receiver) = oneshot::channel::<usize>();
        let sender = Arc::new(StdMutex::new(Some(sender)));
        let first_overlay_accepted = Arc::new(Notify::new());
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handler_sender = sender.clone();
        let handler_overlay_accepted = first_overlay_accepted.clone();
        let handler_seen = seen.clone();
        let handler: TriggerHandler<()> = Arc::new(move |ctx| {
            let handler_sender = handler_sender.clone();
            let handler_overlay_accepted = handler_overlay_accepted.clone();
            let handler_seen = handler_seen.clone();
            Box::pin(async move {
                let call = handler_seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if call == 0 {
                    let overlay =
                        TriggerPolicyOverlay::builder("temporary burst", Duration::from_secs(5))
                            .concurrency_limit(2)
                            .build()
                            .expect("overlay should be valid");
                    ctx.request_policy_overlay(overlay)
                        .expect("overlay should be accepted");
                    handler_overlay_accepted.notify_one();
                } else if let Some(sender) = handler_sender
                    .lock()
                    .expect("sender mutex should not be poisoned")
                    .take()
                {
                    let pressure = ctx
                        .pressure()
                        .expect("trigger pressure should be available in trigger scope");
                    let _ = sender.send(pressure.current_limit);
                }
                Ok(())
            })
        });

        let run_result = __run_service_scope(
            ServiceIdentity::new(
                service_instance_id,
                "overlay_trigger",
                CancellationToken::new(),
                CancellationToken::new(),
            ),
            resources,
            || async move {
                let release_second = Arc::new(Notify::new());
                let runner = TriggerRunner::new(
                    "overlay_trigger",
                    service_instance_id,
                    handler,
                    RestartPolicy::default(),
                    Some(
                        ScalingPolicy::builder()
                            .initial_concurrency(1)
                            .max_concurrency(2)
                            .build(),
                    ),
                );
                let mut host = TwoDispatchHost {
                    emitted: 0,
                    release_second: release_second.clone(),
                };
                let overlay_accepted = first_overlay_accepted.clone();
                tokio::spawn(async move {
                    overlay_accepted.notified().await;
                    release_second.notify_one();
                });
                runner
                    .run_with_host::<(), TwoDispatchHost>(&mut host, Arc::new(()))
                    .await
            },
        )
        .await;

        run_result.expect("trigger should complete");
        assert_eq!(
            receiver
                .await
                .expect("second handler should send observed limit"),
            2
        );
    }

    #[tokio::test]
    async fn trigger_policy_overlay_dispatch_timeout_records_typed_failure() {
        use crate::core::context::{__run_service_scope, DaemonResources, ServiceIdentity};
        use crate::models::TriggerPolicyOverlay;
        use tokio_util::sync::CancellationToken;

        let resources = DaemonResources::new();
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(304));
        let first_overlay_accepted = Arc::new(Notify::new());
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handler_overlay_accepted = first_overlay_accepted.clone();
        let handler_seen = seen.clone();
        let handler: TriggerHandler<()> = Arc::new(move |ctx| {
            let handler_seen = handler_seen.clone();
            let handler_overlay_accepted = handler_overlay_accepted.clone();
            Box::pin(async move {
                let call = handler_seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if call == 0 {
                    let overlay =
                        TriggerPolicyOverlay::builder("temporary timeout", Duration::from_secs(5))
                            .dispatch_timeout(Duration::from_millis(10))
                            .build()
                            .expect("overlay should be valid");
                    ctx.request_policy_overlay(overlay)
                        .expect("overlay should be accepted");
                    handler_overlay_accepted.notify_one();
                    Ok(())
                } else {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    Ok(())
                }
            })
        });

        let run_result = __run_service_scope(
            ServiceIdentity::new(
                service_instance_id,
                "timeout_overlay_trigger",
                CancellationToken::new(),
                CancellationToken::new(),
            ),
            resources.clone(),
            || async move {
                let release_second = Arc::new(Notify::new());
                let runner = TriggerRunner::new(
                    "timeout_overlay_trigger",
                    service_instance_id,
                    handler,
                    RestartPolicy::for_testing(),
                    None,
                );
                let mut host = TwoDispatchHost {
                    emitted: 0,
                    release_second: release_second.clone(),
                };
                let overlay_accepted = first_overlay_accepted.clone();
                tokio::spawn(async move {
                    overlay_accepted.notified().await;
                    release_second.notify_one();
                });
                runner
                    .run_with_host::<(), TwoDispatchHost>(&mut host, Arc::new(()))
                    .await
            },
        )
        .await;

        let error = run_result.expect_err("second dispatch should time out");
        let failure = error
            .downcast_ref::<TriggerDispatchFailure>()
            .expect("timeout should use typed trigger dispatch failure");
        assert_eq!(failure.kind(), TriggerDispatchFailureKind::DispatchTimedOut);
        let snapshot = resources
            .runtime_facts
            .trigger_snapshot(service_instance_id)
            .expect("trigger runtime snapshot should be registered");
        assert_eq!(snapshot.pressure.failed_total, 1);
        assert!(
            snapshot
                .pressure
                .last_error
                .as_deref()
                .is_some_and(|error| error.contains("dispatch_timed_out"))
        );
    }

    #[tokio::test]
    async fn trigger_policy_overlay_dispatch_timeout_is_captured_per_dispatch() {
        use crate::core::context::{__run_service_scope, DaemonResources, ServiceIdentity};
        use crate::models::TriggerPolicyOverlay;
        use tokio_util::sync::CancellationToken;

        let resources = DaemonResources::new();
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(305));
        let first_overlay_accepted = Arc::new(Notify::new());
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let second_handler_started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let handler_overlay_accepted = first_overlay_accepted.clone();
        let handler_second_started = second_handler_started.clone();
        let handler_seen = seen.clone();
        let handler: TriggerHandler<()> = Arc::new(move |ctx| {
            let handler_seen = handler_seen.clone();
            let handler_overlay_accepted = handler_overlay_accepted.clone();
            let handler_second_started = handler_second_started.clone();
            Box::pin(async move {
                let call = handler_seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if call == 0 {
                    let overlay =
                        TriggerPolicyOverlay::builder("captured timeout", Duration::from_secs(5))
                            .dispatch_timeout(Duration::from_millis(10))
                            .build()
                            .expect("overlay should be valid");
                    ctx.request_policy_overlay(overlay)
                        .expect("overlay should be accepted");
                    handler_overlay_accepted.notify_one();
                    Ok(())
                } else {
                    ctx.clear_policy_overlay("should not alter captured dispatch")
                        .expect("clear should be accepted");
                    handler_second_started.store(true, std::sync::atomic::Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    Ok(())
                }
            })
        });

        let run_result = __run_service_scope(
            ServiceIdentity::new(
                service_instance_id,
                "captured_timeout_overlay_trigger",
                CancellationToken::new(),
                CancellationToken::new(),
            ),
            resources,
            || async move {
                let release_second = Arc::new(Notify::new());
                let runner = TriggerRunner::new(
                    "captured_timeout_overlay_trigger",
                    service_instance_id,
                    handler,
                    RestartPolicy::for_testing(),
                    None,
                );
                let mut host = TwoDispatchHost {
                    emitted: 0,
                    release_second: release_second.clone(),
                };
                let overlay_accepted = first_overlay_accepted.clone();
                tokio::spawn(async move {
                    overlay_accepted.notified().await;
                    release_second.notify_one();
                });
                runner
                    .run_with_host::<(), TwoDispatchHost>(&mut host, Arc::new(()))
                    .await
            },
        )
        .await;

        assert!(second_handler_started.load(std::sync::atomic::Ordering::SeqCst));
        let error = run_result.expect_err("captured timeout should still fail after clear");
        let failure = error
            .downcast_ref::<TriggerDispatchFailure>()
            .expect("timeout should use typed trigger dispatch failure");
        assert_eq!(failure.kind(), TriggerDispatchFailureKind::DispatchTimedOut);
    }

    #[tokio::test]
    async fn trigger_runtime_snapshot_records_retry_and_failure_counters() {
        use crate::core::context::{__run_service_scope, DaemonResources, ServiceIdentity};
        use tokio_util::sync::CancellationToken;

        let resources = DaemonResources::new();
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(302));
        let handler: TriggerHandler<()> =
            Arc::new(|_ctx| Box::pin(async { Err(anyhow::anyhow!("retry me")) }));
        let restart_policy = RestartPolicy::builder()
            .initial_delay(Duration::from_millis(1))
            .max_delay(Duration::from_millis(1))
            .jitter_factor(0.0)
            .trigger_max_retries(1)
            .build();

        let run_result = __run_service_scope(
            ServiceIdentity::new(
                service_instance_id,
                "retry_counter_trigger",
                CancellationToken::new(),
                CancellationToken::new(),
            ),
            resources.clone(),
            || async move {
                let runner = TriggerRunner::new(
                    "retry_counter_trigger",
                    service_instance_id,
                    handler,
                    restart_policy,
                    None,
                );
                let mut host = OneShotBlockingHost { emitted: false };
                tokio::time::timeout(
                    Duration::from_millis(500),
                    runner.run_with_host::<(), OneShotBlockingHost>(&mut host, Arc::new(())),
                )
                .await
                .expect("retry exhaustion should finish before timeout")
            },
        )
        .await;

        assert!(run_result.is_err());
        let snapshot = resources
            .runtime_facts
            .trigger_snapshot(service_instance_id)
            .expect("trigger runtime snapshot should be registered");
        assert_eq!(snapshot.pressure.dispatched_total, 1);
        assert_eq!(snapshot.pressure.completed_total, 0);
        assert_eq!(snapshot.pressure.failed_total, 1);
        assert_eq!(snapshot.pressure.retry_total, 1);
        assert!(snapshot.pressure.last_error.is_some());
        assert!(snapshot.pressure.last_error_at.is_some());
    }

    #[tokio::test]
    async fn dispatch_panic_propagates_typed_panic_failure() {
        use crate::core::context::{__run_service_scope, DaemonResources, ServiceIdentity};
        use tokio_util::sync::CancellationToken;

        let handler: TriggerHandler<()> = Arc::new(|_ctx| {
            Box::pin(async {
                if std::hint::black_box(true) {
                    panic!("handler panicked inside dispatch task");
                }
                Ok(())
            })
        });
        let runner = TriggerRunner::new(
            "panicking_dispatch_trigger",
            ServiceInstanceId::new(uuid::Uuid::from_u128(201)),
            handler,
            RestartPolicy::for_testing(),
            None,
        );

        let result = __run_service_scope(
            ServiceIdentity::new(
                ServiceInstanceId::new(uuid::Uuid::from_u128(201)),
                "panicking_dispatch_trigger",
                CancellationToken::new(),
                CancellationToken::new(),
            ),
            DaemonResources::new(),
            || async move {
                let mut host = OneShotBlockingHost { emitted: false };
                let target = Arc::new(());
                tokio::time::timeout(
                    Duration::from_millis(500),
                    runner.run_with_host::<(), OneShotBlockingHost>(&mut host, target),
                )
                .await
                .expect("dispatch panic was not propagated to run_with_host")
            },
        )
        .await;

        let error = result.expect_err("run_with_host should fail when dispatch panics");
        let failure = error
            .downcast_ref::<TriggerDispatchFailure>()
            .expect("dispatch panic should use typed trigger dispatch failure");
        assert_eq!(
            failure.kind(),
            TriggerDispatchFailureKind::DispatchTaskPanic
        );
    }

    #[tokio::test]
    async fn retry_interrupted_by_shutdown_is_not_reported_as_dispatch_failure() {
        use crate::core::context::{__run_service_scope, DaemonResources, ServiceIdentity};
        use tokio_util::sync::CancellationToken;

        let handler_started = Arc::new(Notify::new());
        let handler_started_for_handler = handler_started.clone();
        let handler: TriggerHandler<()> = Arc::new(move |_ctx| {
            let handler_started = handler_started_for_handler.clone();
            Box::pin(async move {
                handler_started.notify_one();
                Err(anyhow::anyhow!("transient handler failure"))
            })
        });
        let restart_policy = RestartPolicy::builder()
            .initial_delay(Duration::from_secs(60))
            .max_delay(Duration::from_secs(60))
            .jitter_factor(0.0)
            .build();
        let runner = TriggerRunner::new(
            "shutdown_interrupted_retry_trigger",
            ServiceInstanceId::new(uuid::Uuid::from_u128(202)),
            handler,
            restart_policy,
            None,
        );
        let cancellation_token = CancellationToken::new();
        let cancellation_for_scope = cancellation_token.clone();

        let task = tokio::spawn(__run_service_scope(
            ServiceIdentity::new(
                ServiceInstanceId::new(uuid::Uuid::from_u128(202)),
                "shutdown_interrupted_retry_trigger",
                cancellation_token,
                CancellationToken::new(),
            ),
            DaemonResources::new(),
            || async move {
                let mut host = OneShotBlockingHost { emitted: false };
                let target = Arc::new(());
                runner
                    .run_with_host::<(), OneShotBlockingHost>(&mut host, target)
                    .await
            },
        ));

        tokio::time::timeout(Duration::from_millis(500), handler_started.notified())
            .await
            .expect("handler should start before shutdown");
        cancellation_for_scope.cancel();

        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("runner should exit after shutdown")
            .expect("service scope task should not panic");
        assert!(
            result.is_ok(),
            "shutdown-interrupted retry should exit cleanly: {result:?}"
        );
    }

    #[tokio::test]
    async fn reload_transition_cancels_in_flight_dispatch_without_false_failure() {
        use crate::core::context::{__run_service_scope, DaemonResources, ServiceIdentity};
        use tokio_util::sync::CancellationToken;

        let handler_started = Arc::new(Notify::new());
        let handler_started_for_handler = handler_started.clone();
        let handler: TriggerHandler<()> = Arc::new(move |_ctx| {
            let handler_started = handler_started_for_handler.clone();
            Box::pin(async move {
                handler_started.notify_one();
                futures::future::pending::<()>().await;
                Ok(())
            })
        });
        let runner = TriggerRunner::new(
            "reload_cancels_dispatch_trigger",
            ServiceInstanceId::new(uuid::Uuid::from_u128(203)),
            handler,
            RestartPolicy::for_testing(),
            None,
        );
        let reload_token = CancellationToken::new();
        let reload_for_scope = reload_token.clone();

        let task = tokio::spawn(__run_service_scope(
            ServiceIdentity::new(
                ServiceInstanceId::new(uuid::Uuid::from_u128(203)),
                "reload_cancels_dispatch_trigger",
                CancellationToken::new(),
                reload_token,
            ),
            DaemonResources::new(),
            || async move {
                let mut host = OneShotReloadHost { emitted: false };
                let target = Arc::new(());
                runner
                    .run_with_host::<(), OneShotReloadHost>(&mut host, target)
                    .await
            },
        ));

        tokio::time::timeout(Duration::from_millis(500), handler_started.notified())
            .await
            .expect("handler should start before reload cancellation");
        reload_for_scope.cancel();

        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("runner should exit after reload")
            .expect("service scope task should not panic");
        assert!(
            result.is_ok(),
            "reload-cancelled dispatch should exit cleanly: {result:?}"
        );
    }

    #[tokio::test]
    async fn stop_transition_drains_in_flight_dispatch_failure() {
        use crate::core::context::{__run_service_scope, DaemonResources, ServiceIdentity};
        use tokio_util::sync::CancellationToken;

        let handler_started = Arc::new(Notify::new());
        let release_handler = Arc::new(Notify::new());
        let handler_started_for_handler = handler_started.clone();
        let release_for_handler = release_handler.clone();
        let handler: TriggerHandler<()> = Arc::new(move |_ctx| {
            let handler_started = handler_started_for_handler.clone();
            let release_handler = release_for_handler.clone();
            Box::pin(async move {
                handler_started.notify_one();
                release_handler.notified().await;
                Err(anyhow::anyhow!("late dispatch failure"))
            })
        });
        let restart_policy = RestartPolicy::builder()
            .initial_delay(Duration::from_millis(1))
            .max_delay(Duration::from_millis(1))
            .jitter_factor(0.0)
            .trigger_max_retries(1)
            .build();
        let runner = TriggerRunner::new(
            "stop_drains_dispatch_trigger",
            ServiceInstanceId::new(uuid::Uuid::from_u128(204)),
            handler,
            restart_policy,
            None,
        );
        let stop_emitted = Arc::new(Notify::new());
        let stop_seen = stop_emitted.clone();

        let task = tokio::spawn(__run_service_scope(
            ServiceIdentity::new(
                ServiceInstanceId::new(uuid::Uuid::from_u128(204)),
                "stop_drains_dispatch_trigger",
                CancellationToken::new(),
                CancellationToken::new(),
            ),
            DaemonResources::new(),
            || async move {
                let mut host = DispatchThenStopHost {
                    emitted: false,
                    stop_emitted,
                };
                let target = Arc::new(());
                runner
                    .run_with_host::<(), DispatchThenStopHost>(&mut host, target)
                    .await
            },
        ));

        tokio::time::timeout(Duration::from_millis(500), handler_started.notified())
            .await
            .expect("handler should start before stop");
        tokio::time::timeout(Duration::from_millis(500), stop_seen.notified())
            .await
            .expect("host should emit Stop before handler is released");
        release_handler.notify_one();

        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("runner should finish draining stop dispatch")
            .expect("service scope task should not panic");
        let error = result.expect_err("normal Stop should propagate drained dispatch failure");
        let failure = error
            .downcast_ref::<TriggerDispatchFailure>()
            .expect("drained dispatch failure should stay typed");
        assert_eq!(
            failure.kind(),
            TriggerDispatchFailureKind::HandlerRetryExhausted
        );
    }

    #[tokio::test]
    async fn shutdown_drains_in_flight_dispatch_before_exit() {
        use crate::core::context::{__run_service_scope, DaemonResources, ServiceIdentity};
        use crate::core::diagnostics::{DiagnosticsStore, RuntimeLane};
        use crate::models::TriggerPolicyOverlay;
        use std::sync::atomic::{AtomicBool, Ordering};
        use tokio_util::sync::CancellationToken;

        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(206));
        let handler_started = Arc::new(Notify::new());
        let release_handler = Arc::new(Notify::new());
        let handler_finished = Arc::new(AtomicBool::new(false));

        let handler_started_for_handler = handler_started.clone();
        let release_for_handler = release_handler.clone();
        let handler_finished_for_handler = handler_finished.clone();
        let handler: TriggerHandler<()> = Arc::new(move |ctx| {
            let handler_started = handler_started_for_handler.clone();
            let release_handler = release_for_handler.clone();
            let handler_finished = handler_finished_for_handler.clone();
            Box::pin(async move {
                let overlay =
                    TriggerPolicyOverlay::builder("shutdown cleanup", Duration::from_secs(5))
                        .concurrency_limit(1)
                        .build()
                        .expect("overlay should be valid");
                ctx.request_policy_overlay(overlay)
                    .expect("overlay should be accepted");
                handler_started.notify_one();
                release_handler.notified().await;
                handler_finished.store(true, Ordering::SeqCst);
                Ok(())
            })
        });
        let cancellation_token = CancellationToken::new();
        let cancellation_for_scope = cancellation_token.clone();
        let diagnostics = DiagnosticsStore::new();
        let diagnostics_handle = diagnostics.register_generation(
            service_instance_id,
            "shutdown_drains_dispatch_trigger",
            1,
            RuntimeLane::Standard,
        );
        let resources = DaemonResources::new();

        let task = tokio::spawn(__run_service_scope(
            ServiceIdentity::new_with_diagnostics(
                service_instance_id,
                "shutdown_drains_dispatch_trigger",
                cancellation_token,
                CancellationToken::new(),
                diagnostics_handle,
            ),
            resources.clone(),
            move || async move {
                let runner = TriggerRunner::new(
                    "shutdown_drains_dispatch_trigger",
                    service_instance_id,
                    handler,
                    RestartPolicy::for_testing(),
                    None,
                );
                let mut host = OneShotBlockingHost { emitted: false };
                let target = Arc::new(());
                runner
                    .run_with_host::<(), OneShotBlockingHost>(&mut host, target)
                    .await
            },
        ));

        tokio::time::timeout(Duration::from_millis(500), handler_started.notified())
            .await
            .expect("handler should start before shutdown");
        cancellation_for_scope.cancel();
        tokio::task::yield_now().await;
        assert!(
            !task.is_finished(),
            "runner should wait for in-flight dispatch during shutdown drain"
        );

        release_handler.notify_one();

        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("runner should exit after drained dispatch completes")
            .expect("service scope task should not panic");
        assert!(
            result.is_ok(),
            "drained shutdown should exit cleanly: {result:?}"
        );
        assert!(handler_finished.load(Ordering::SeqCst));

        let snapshot: crate::models::DaemonDiagnosticsSnapshot = diagnostics.snapshot().into();
        let generation = snapshot
            .generations
            .iter()
            .find(|generation| {
                generation.service_instance_id == ServiceInstanceId::new(uuid::Uuid::from_u128(206))
            })
            .expect("trigger generation diagnostics should be projected");
        let boundary = generation
            .aggregate
            .shutdown_boundary
            .last_outcome
            .expect("trigger drain outcome should be recorded");
        assert_eq!(
            boundary.boundary,
            crate::models::DiagnosticShutdownBoundaryKind::TriggerDispatchDrain
        );
        assert_eq!(
            boundary.result,
            crate::models::DiagnosticShutdownBoundaryResultKind::Completed
        );
        assert_eq!(boundary.completed, 1);
        assert_eq!(boundary.residual, 0);
        assert!(
            !resources
                .trigger_policy_overlays
                .has_active_overlay(service_instance_id, 1),
            "overlay generation guard should remove active overlay after shutdown drain"
        );
    }
}
