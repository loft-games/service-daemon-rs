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

use crate::models::policy::{RestartPolicy, ScalingPolicy};
use crate::models::service::ServiceId;
use crate::models::trigger::TriggerHandler;

use self::interceptors::{RetryInterceptor, TracingInterceptor};

pub struct TriggerRunner<P: Send + Sync + 'static> {
    /// Human-readable name of this trigger service.
    name: &'static str,
    /// The `ServiceId` of the trigger service.
    service_id: ServiceId,
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
        service_id: ServiceId,
        handler: TriggerHandler<P>,
        restart_policy: RestartPolicy,
        scaling: Option<ScalingPolicy>,
    ) -> Self {
        let initial = scaling.map_or(1, |sp| sp.initial_concurrency());
        Self {
            name,
            service_id,
            instance_counter: AtomicU64::new(0),
            handler,
            interceptors: vec![
                Arc::new(TracingInterceptor),
                Arc::new(RetryInterceptor {
                    policy: restart_policy,
                }),
            ],
            scaling,
            semaphore: Arc::new(Semaphore::new(initial)),
            current_limit: Arc::new(AtomicUsize::new(initial)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::BoxFuture;
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
            ServiceId::new(200),
            handler,
            restart_policy,
            None,
        );

        let result = __run_service_scope(
            ServiceIdentity::new(
                ServiceId::new(200),
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
            ServiceId::new(201),
            handler,
            RestartPolicy::for_testing(),
            None,
        );

        let result = __run_service_scope(
            ServiceIdentity::new(
                ServiceId::new(201),
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
            ServiceId::new(202),
            handler,
            restart_policy,
            None,
        );
        let cancellation_token = CancellationToken::new();
        let cancellation_for_scope = cancellation_token.clone();

        let task = tokio::spawn(__run_service_scope(
            ServiceIdentity::new(
                ServiceId::new(202),
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
            ServiceId::new(203),
            handler,
            RestartPolicy::for_testing(),
            None,
        );
        let reload_token = CancellationToken::new();
        let reload_for_scope = reload_token.clone();

        let task = tokio::spawn(__run_service_scope(
            ServiceIdentity::new(
                ServiceId::new(203),
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
            ServiceId::new(204),
            handler,
            restart_policy,
            None,
        );
        let stop_emitted = Arc::new(Notify::new());
        let stop_seen = stop_emitted.clone();

        let task = tokio::spawn(__run_service_scope(
            ServiceIdentity::new(
                ServiceId::new(204),
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
        use std::sync::atomic::{AtomicBool, Ordering};
        use tokio_util::sync::CancellationToken;

        let handler_started = Arc::new(Notify::new());
        let release_handler = Arc::new(Notify::new());
        let handler_finished = Arc::new(AtomicBool::new(false));

        let handler_started_for_handler = handler_started.clone();
        let release_for_handler = release_handler.clone();
        let handler_finished_for_handler = handler_finished.clone();
        let handler: TriggerHandler<()> = Arc::new(move |_ctx| {
            let handler_started = handler_started_for_handler.clone();
            let release_handler = release_for_handler.clone();
            let handler_finished = handler_finished_for_handler.clone();
            Box::pin(async move {
                handler_started.notify_one();
                release_handler.notified().await;
                handler_finished.store(true, Ordering::SeqCst);
                Ok(())
            })
        });
        let runner = TriggerRunner::new(
            "shutdown_drains_dispatch_trigger",
            ServiceId::new(206),
            handler,
            RestartPolicy::for_testing(),
            None,
        );
        let cancellation_token = CancellationToken::new();
        let cancellation_for_scope = cancellation_token.clone();
        let diagnostics = DiagnosticsStore::new();
        let diagnostics_handle = diagnostics.register_generation(
            ServiceId::new(206),
            "shutdown_drains_dispatch_trigger",
            1,
            RuntimeLane::Standard,
        );

        let task = tokio::spawn(__run_service_scope(
            ServiceIdentity::new_with_diagnostics(
                ServiceId::new(206),
                "shutdown_drains_dispatch_trigger",
                cancellation_token,
                CancellationToken::new(),
                diagnostics_handle,
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
            .find(|generation| generation.service_id == ServiceId::new(206))
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
    }
}
