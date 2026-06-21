//! ServiceDaemon - the main orchestrator for managed services.
//!
//! This module is split into submodules for better organization:
//! - `builder`: ServiceDaemon construction and registry assembly.
//! - `policy`: Restart policy configuration.
//! - `provider_graph`: Provider graph validation and eager provider startup.
//! - `runner`: Service spawning and lifecycle management.
//! - `runtime`: Runtime creation, probes, and shutdown helpers.
//! - `startup_preflight`: Shared startup validation, provider init, and runtime preparation.
//! - `startup_pipeline`: Production startup orchestration after shared preflight.

mod builder;
mod parts;
mod policy;
mod provider_graph;
mod runner;
mod runtime;
#[cfg(feature = "simulation")]
mod simulation_startup;
mod startup_pipeline;
mod startup_preflight;

use std::collections::HashMap;
use std::future::pending;
use std::sync::Arc;
#[cfg(feature = "simulation")]
use std::time::Duration;
#[cfg(all(feature = "simulation", test))]
use std::time::Instant;
use tokio::runtime::Runtime;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument};

#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};

use crate::core::context::DaemonResources;
use crate::core::diagnostics::DiagnosticsStore;
#[cfg(any(unix, feature = "simulation"))]
use crate::models::ServiceError;
use crate::models::{
    DaemonDiagnosticsSnapshot, DaemonRuntimeSnapshot, ReadinessSnapshot, Result as ServiceResult,
    SchedulingAdvisoryProfile, ServiceDescription, ServiceId, ServiceRuntimeSnapshot,
    ServiceStatus, TriggerRuntimeSnapshot,
};

pub use builder::ServiceDaemonBuilder;
pub use policy::{RestartPolicy, RestartPolicyBuilder};
use runtime::HighPriorityCapacityPlan;
use startup_pipeline::StartupError;

// ---------------------------------------------------------------------------
// ServiceDaemonHandle -- lightweight status query interface
// ---------------------------------------------------------------------------

/// A handle to the ServiceDaemon that can be used to query status and interact with services.
#[derive(Clone)]
pub struct ServiceDaemonHandle {
    resources: Arc<DaemonResources>,
    diagnostics: Arc<DiagnosticsStore>,
    shutdown_token: CancellationToken,
}

impl ServiceDaemonHandle {
    /// Get the current status of a service by its `ServiceId`.
    pub async fn get_service_status(&self, id: &ServiceId) -> ServiceStatus {
        self.resources
            .status_plane
            .get(id)
            .map(|s| s.clone())
            .unwrap_or(ServiceStatus::Terminated)
    }

    /// Return a read-only snapshot of daemon diagnostics.
    pub fn diagnostics_snapshot(&self) -> DaemonDiagnosticsSnapshot {
        self.diagnostics.snapshot().into()
    }

    /// Return read-only daemon runtime facts.
    pub fn runtime(&self) -> DaemonRuntimeSnapshot {
        self.resources
            .runtime_facts
            .daemon_snapshot(self.shutdown_token.is_cancelled())
    }

    /// Return a facts-only readiness grouping.
    pub fn runtime_readiness(&self) -> ReadinessSnapshot {
        self.resources
            .runtime_facts
            .readiness_snapshot(|service_id| self.status_for_snapshot(service_id))
    }

    /// Return read-only runtime facts for all registered services.
    pub fn runtime_services(&self) -> Vec<ServiceRuntimeSnapshot> {
        self.resources
            .runtime_facts
            .service_snapshots(|service_id| self.status_for_snapshot(service_id))
    }

    /// Return read-only runtime facts for a service.
    pub fn runtime_service(&self, id: ServiceId) -> Option<ServiceRuntimeSnapshot> {
        self.resources
            .runtime_facts
            .service_snapshot(id, |service_id| self.status_for_snapshot(service_id))
    }

    /// Return read-only runtime facts for all observed triggers.
    pub fn runtime_triggers(&self) -> Vec<TriggerRuntimeSnapshot> {
        self.resources.runtime_facts.trigger_snapshots()
    }

    /// Return read-only runtime facts for an observed trigger.
    pub fn runtime_trigger(&self, id: ServiceId) -> Option<TriggerRuntimeSnapshot> {
        self.resources.runtime_facts.trigger_snapshot(id)
    }

    fn status_for_snapshot(&self, id: ServiceId) -> ServiceStatus {
        self.resources
            .status_plane
            .get(&id)
            .map(|status| status.clone())
            .unwrap_or(ServiceStatus::Initializing)
    }
}

// ---------------------------------------------------------------------------
// ServiceDaemon -- Infallible Builder pattern
// ---------------------------------------------------------------------------

/// The main orchestrator for managed services.
///
/// `ServiceDaemon` acts as both a lifecycle manager and a control handle.
/// After calling [`run()`](ServiceDaemon::run), the daemon starts services
/// in the background and returns control to the caller. Use
/// [`wait()`](ServiceDaemon::wait) to block until shutdown, or
/// [`shutdown()`](ServiceDaemon::shutdown) to trigger graceful termination.
///
/// # Examples
/// ```rust,ignore
/// // Non-blocking start, then wait for Ctrl+C:
/// let mut daemon = ServiceDaemon::builder().build();
/// daemon.run().await;
/// daemon.wait().await?;
///
/// // Hierarchical integration with external CancellationToken:
/// let root_token = CancellationToken::new();
/// let mut daemon = ServiceDaemon::builder()
///     .with_cancel_token(root_token.clone())
///     .build();
/// daemon.run().await;
/// // ... other work using root_token ...
/// daemon.wait().await?;
/// ```
pub struct ServiceDaemon {
    services: Vec<ServiceDescription>,
    running_tasks: Arc<Mutex<HashMap<ServiceId, JoinHandle<()>>>>,
    restart_policy: RestartPolicy,
    cancellation_token: CancellationToken,
    /// Dedicated runtime for supervisor and control-plane work.
    control_runtime: Option<Runtime>,
    high_priority_capacity: HighPriorityCapacityPlan,
    /// Shared runtime lazily created for HighPriority service bodies.
    high_priority_runtime: Option<Runtime>,
    runtime_probe_tasks: Vec<JoinHandle<()>>,
    adaptive_recommendation_task: Option<JoinHandle<()>>,
    scheduling_advisory_profile: SchedulingAdvisoryProfile,
    /// Optional external token for hierarchical lifecycle management.
    /// When cancelled, the daemon treats it as a shutdown signal.
    external_cancel_token: Option<CancellationToken>,
    /// Instance-owned resources (Status Plane, Shelf, Signals)
    resources: Arc<DaemonResources>,
    diagnostics: Arc<DiagnosticsStore>,
    isolated_startup_permits: Arc<Semaphore>,
}

impl Drop for ServiceDaemon {
    fn drop(&mut self) {
        self.abort_adaptive_recommendation_loop();
        self.shutdown_high_priority_runtime_detached();
        self.shutdown_control_runtime_detached();
    }
}

impl ServiceDaemon {
    /// Start building a new `ServiceDaemon`.
    #[must_use]
    pub fn builder() -> ServiceDaemonBuilder {
        ServiceDaemonBuilder::new()
    }

    /// Get the cancellation token for this daemon.
    pub fn cancel_token(&self) -> tokio_util::sync::CancellationToken {
        self.cancellation_token.clone()
    }

    /// Get a handle to the daemon for querying status and diagnostics.
    pub fn handle(&self) -> ServiceDaemonHandle {
        ServiceDaemonHandle {
            resources: self.resources.clone(),
            diagnostics: self.diagnostics.clone(),
            shutdown_token: self.cancellation_token.clone(),
        }
    }

    /// Get the current status of a service by its `ServiceId`.
    pub async fn get_service_status(&self, id: &ServiceId) -> ServiceStatus {
        self.handle().get_service_status(id).await
    }

    /// Return a read-only snapshot of daemon diagnostics.
    pub fn diagnostics_snapshot(&self) -> DaemonDiagnosticsSnapshot {
        self.diagnostics.snapshot().into()
    }

    /// Return read-only daemon runtime facts.
    pub fn runtime(&self) -> DaemonRuntimeSnapshot {
        self.handle().runtime()
    }

    /// Return a facts-only readiness grouping.
    pub fn runtime_readiness(&self) -> ReadinessSnapshot {
        self.handle().runtime_readiness()
    }

    /// Return read-only runtime facts for all registered services.
    pub fn runtime_services(&self) -> Vec<ServiceRuntimeSnapshot> {
        self.handle().runtime_services()
    }

    /// Return read-only runtime facts for a service.
    pub fn runtime_service(&self, id: ServiceId) -> Option<ServiceRuntimeSnapshot> {
        self.handle().runtime_service(id)
    }

    /// Return read-only runtime facts for all observed triggers.
    pub fn runtime_triggers(&self) -> Vec<TriggerRuntimeSnapshot> {
        self.handle().runtime_triggers()
    }

    /// Return read-only runtime facts for an observed trigger.
    pub fn runtime_trigger(&self, id: ServiceId) -> Option<TriggerRuntimeSnapshot> {
        self.handle().runtime_trigger(id)
    }

    /// Start the daemon in the background (non-blocking).
    ///
    /// This method spawns all registered services using wave-based priorities
    /// and returns immediately. The daemon continues running in the background.
    ///
    /// Use [`wait()`](ServiceDaemon::wait) to block until a shutdown signal,
    /// or [`shutdown()`](ServiceDaemon::shutdown) to trigger graceful termination.
    #[instrument(skip(self))]
    pub async fn run(&mut self) -> &mut Self {
        if self.services.is_empty() {
            info!("ServiceDaemon has no services to run. Daemon started in idle mode.");
        }

        if let Err(err) = self.run_startup_pipeline().await {
            match err {
                StartupError::ProviderGraph(err) => {
                    tracing::error!(error = %err, "ServiceDaemon provider dependency graph validation failed");
                }
                StartupError::EagerProviderInit(err) => {
                    tracing::error!(error = %err, "ServiceDaemon eager provider initialization failed");
                }
                StartupError::ControlRuntime(err) => {
                    tracing::error!(error = %err, "ServiceDaemon control runtime creation failed");
                }
                StartupError::HighPriorityRuntime(err) => {
                    tracing::error!(error = %err, "ServiceDaemon high-priority runtime creation failed");
                }
                StartupError::StartupOrchestration(err) => {
                    tracing::error!(error = ?err, "ServiceDaemon startup orchestration failed");
                }
            }
            self.shutdown();
            return self;
        }

        info!(
            "ServiceDaemon running with {} service(s).",
            self.services.len()
        );

        self
    }

    /// Wait for the daemon to stop.
    ///
    /// This method blocks until one of the following events occurs:
    /// - An OS signal is received (SIGINT / SIGTERM / Ctrl+C).
    /// - The internal cancellation token is cancelled (via [`shutdown()`](ServiceDaemon::shutdown)).
    /// - An external cancellation token is cancelled (if provided via
    ///   [`with_cancel_token()`](ServiceDaemonBuilder::with_cancel_token)).
    ///
    /// After the trigger event, this method performs a graceful shutdown
    /// of all services using wave-based priorities.
    ///
    /// # Errors
    /// - `ServiceError::InternalError(...)` if the daemon cannot register a
    ///   shutdown signal listener (for example, `SIGINT` / `SIGTERM` on Unix,
    ///   or `Ctrl+C` on non-Unix platforms).
    ///
    /// Runtime service/provider failures are handled by the runner and
    /// shutdown path rather than being returned from `wait()`.
    ///
    /// # Signal Guard (Layer 1 Defense)
    /// If signal handler registration fails, this method returns `Err`
    /// immediately to prevent an uncontrollable daemon.
    #[instrument(skip(self))]
    pub async fn wait(&mut self) -> ServiceResult<()> {
        // Wait for shutdown signal (Ctrl+C, SIGTERM, or token cancellation)
        #[cfg(unix)]
        {
            let mut sigint = signal(SignalKind::interrupt()).map_err(|e| {
                ServiceError::InternalError(format!("Failed to setup SIGINT: {}", e))
            })?;
            let mut sigterm = signal(SignalKind::terminate()).map_err(|e| {
                ServiceError::InternalError(format!("Failed to setup SIGTERM: {}", e))
            })?;

            tokio::select! {
                _ = sigint.recv() => {
                    info!("Received SIGINT, shutting down...");
                }
                _ = sigterm.recv() => {
                    info!("Received SIGTERM, shutting down...");
                }
                _ = self.cancellation_token.cancelled() => {
                    info!("Received internal cancellation signal, shutting down...");
                }
                _ = Self::wait_external_token(&self.external_cancel_token) => {
                    info!("Received external cancellation signal, shutting down...");
                }
            }
        }

        #[cfg(not(unix))]
        {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("Received Ctrl+C, shutting down...");
                }
                _ = self.cancellation_token.cancelled() => {
                    info!("Received internal cancellation signal, shutting down...");
                }
                _ = Self::wait_external_token(&self.external_cancel_token) => {
                    info!("Received external cancellation signal, shutting down...");
                }
            }
        }

        // Graceful shutdown
        self.do_shutdown().await;

        Ok(())
    }

    /// Trigger graceful shutdown of the daemon.
    ///
    /// This cancels the internal `CancellationToken`, which will cause
    /// [`wait()`](ServiceDaemon::wait) to proceed with the shutdown sequence.
    /// If an external token was provided, it is also cancelled to propagate
    /// the shutdown signal to other components sharing that token.
    pub fn shutdown(&self) {
        info!("ServiceDaemon::shutdown() called, triggering graceful termination...");
        self.cancellation_token.cancel();
        // Propagate shutdown to external token if present
        if let Some(ref external) = self.external_cancel_token {
            external.cancel();
        }
    }

    /// Internal helper: perform the actual graceful shutdown sequence.
    async fn do_shutdown(&mut self) {
        if let Some(control_runtime) = self.control_runtime.as_ref() {
            let services = clone_service_descriptions(&self.services);
            let running_tasks = self.running_tasks.clone();
            let resources = self.resources.clone();
            let cancellation_token = self.cancellation_token.clone();
            let wave_stop_timeout = self.restart_policy.wave_stop_timeout;
            let shutdown = control_runtime.spawn(async move {
                runner::stop_all_services(
                    &services,
                    running_tasks,
                    resources,
                    cancellation_token,
                    wave_stop_timeout,
                )
                .await;
            });

            if let Err(err) = shutdown.await {
                tracing::error!(error = ?err, "ServiceDaemon shutdown orchestration failed");
            }
        } else {
            runner::stop_all_services(
                &self.services,
                self.running_tasks.clone(),
                self.resources.clone(),
                self.cancellation_token.clone(),
                self.restart_policy.wave_stop_timeout,
            )
            .await;
        }

        self.stop_adaptive_recommendation_loop().await;
        self.stop_runtime_probes().await;
        self.shutdown_high_priority_runtime();
        self.shutdown_control_runtime();

        #[cfg(feature = "diagnostics")]
        emit_shutdown_topology();

        info!("ServiceDaemon stopped.");
    }

    /// Internal helper: wait on an external CancellationToken if present.
    /// If no external token was provided, this future never resolves.
    async fn wait_external_token(token: &Option<CancellationToken>) {
        match token {
            Some(t) => t.cancelled().await,
            None => pending().await,
        }
    }

    /// Run for a limited duration (for testing).
    #[cfg(feature = "simulation")]
    #[instrument(skip(self))]
    pub async fn run_for_duration(mut self, duration: Duration) -> ServiceResult<()> {
        // Use testing policy with shorter delays
        let test_policy = RestartPolicy::for_testing();

        self.run_simulation_startup(test_policy).await?;

        tokio::time::sleep(duration).await;

        runner::stop_all_services(
            &self.services,
            self.running_tasks.clone(),
            self.resources.clone(),
            self.cancellation_token.clone(),
            test_policy.wave_stop_timeout,
        )
        .await;

        self.stop_adaptive_recommendation_loop().await;
        self.stop_runtime_probes().await;
        self.shutdown_high_priority_runtime_detached();
        self.shutdown_control_runtime_detached();

        Ok(())
    }
}

fn clone_service_descriptions(services: &[ServiceDescription]) -> Vec<ServiceDescription> {
    services
        .iter()
        .map(|service| ServiceDescription {
            id: service.id,
            entry: service.entry,
            cancellation_token: service.cancellation_token.clone(),
        })
        .collect()
}

#[cfg(feature = "diagnostics")]
fn emit_shutdown_topology() {
    if let Some(mermaid) = super::topology_collector::export_mermaid() {
        tracing::info!(
            target: "service_daemon::diagnostics",
            topology_mermaid = %mermaid,
            "behavioral topology exported during shutdown"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        ProviderEntry, ProviderInitError, Registry, ServiceEntry, ServiceParam, ServiceScheduling,
    };
    use crate::{TT::*, provider, service, trigger};
    use std::any::TypeId;
    #[cfg(feature = "diagnostics")]
    use std::collections::BTreeMap;
    use std::num::NonZeroUsize;
    use std::sync::atomic::{AtomicU32, Ordering};
    #[cfg(feature = "diagnostics")]
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::Duration;
    use tracing::debug;
    #[cfg(feature = "diagnostics")]
    use tracing::field::{Field, Visit};
    #[cfg(feature = "diagnostics")]
    use tracing_subscriber::Layer;
    #[cfg(feature = "diagnostics")]
    use tracing_subscriber::layer::Context;
    #[cfg(feature = "diagnostics")]
    use tracing_subscriber::prelude::*;

    use super::provider_graph::validate_dependency_graph;
    use super::runtime::ISOLATED_STARTUP_CONCURRENCY_LIMIT;

    /// Helper: Create an isolated registry that filters out all auto-registered services.
    fn isolated_registry() -> Registry {
        Registry::builder().with_tag("__test_isolation__").build()
    }

    fn noop_service(
        _: CancellationToken,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    static STANDARD_TEST_ENTRY: ServiceEntry = ServiceEntry {
        name: "standard_test_service",
        module: "test",
        params: &[],
        wrapper: noop_service,
        watcher: None,
        priority: 50,
        scheduling: ServiceScheduling::Standard,
        tags: &["__unit_runtime_standard__"],
    };

    static HIGH_PRIORITY_TEST_ENTRY: ServiceEntry = ServiceEntry {
        name: "high_priority_test_service",
        module: "test",
        params: &[],
        wrapper: noop_service,
        watcher: None,
        priority: 50,
        scheduling: ServiceScheduling::HighPriority,
        tags: &["__unit_runtime_high_priority__"],
    };

    static ISOLATED_TEST_ENTRY: ServiceEntry = ServiceEntry {
        name: "isolated_test_service",
        module: "test",
        params: &[],
        wrapper: noop_service,
        watcher: None,
        priority: 50,
        scheduling: ServiceScheduling::Isolated,
        tags: &["__unit_runtime_isolated__"],
    };

    fn test_service(id: usize, entry: &'static ServiceEntry) -> ServiceDescription {
        ServiceDescription {
            id: ServiceId::new(id),
            entry,
            cancellation_token: CancellationToken::new(),
        }
    }

    #[cfg(feature = "diagnostics")]
    #[derive(Clone, Default)]
    struct CapturedTraceFields {
        events: Arc<StdMutex<Vec<BTreeMap<String, String>>>>,
    }

    #[cfg(feature = "diagnostics")]
    #[derive(Default)]
    struct TraceFieldVisitor {
        fields: BTreeMap<String, String>,
    }

    #[cfg(feature = "diagnostics")]
    impl Visit for TraceFieldVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.fields
                .insert(field.name().to_string(), format!("{value:?}"));
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }
    }

    #[cfg(feature = "diagnostics")]
    impl<S> Layer<S> for CapturedTraceFields
    where
        S: tracing::Subscriber,
    {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            let mut visitor = TraceFieldVisitor::default();
            event.record(&mut visitor);
            self.events
                .lock()
                .unwrap_or_else(|err| panic!("trace capture lock poisoned: {err}"))
                .push(visitor.fields);
        }
    }

    /// A no-op initializer suitable for fake `ProviderEntry` values in graph tests.
    fn noop_init(
        _: RestartPolicy,
        _: tokio_util::sync::CancellationToken,
    ) -> futures::future::BoxFuture<'static, Result<(), ProviderInitError>> {
        Box::pin(async { Ok(()) })
    }

    /// Leak a params slice to satisfy `&'static [ServiceParam]` without a const context.
    fn leaked_params(params: Vec<ServiceParam>) -> &'static [ServiceParam] {
        Box::leak(params.into_boxed_slice())
    }

    fn non_zero(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("test worker count should be non-zero")
    }

    #[service(tags = ["__unit_high_priority_capacity_primary__"], scheduling = HighPriority)]
    async fn capacity_primary_high_priority_service() -> anyhow::Result<()> {
        Ok(())
    }

    #[service(tags = ["__unit_high_priority_capacity_primary__"], scheduling = Standard)]
    async fn capacity_primary_standard_service() -> anyhow::Result<()> {
        Ok(())
    }

    #[service(tags = ["__unit_high_priority_capacity_infra__"], scheduling = HighPriority)]
    async fn capacity_infra_high_priority_service() -> anyhow::Result<()> {
        Ok(())
    }

    #[provider(Notify)]
    pub struct CapacitySignal;

    #[service(tags = ["__unit_high_priority_capacity_parity__"], scheduling = HighPriority)]
    async fn capacity_parity_high_priority_service() -> anyhow::Result<()> {
        Ok(())
    }

    #[trigger(
        Event(CapacitySignal),
        tags = ["__unit_high_priority_capacity_parity__"],
        scheduling = HighPriority
    )]
    async fn capacity_parity_high_priority_trigger() -> anyhow::Result<()> {
        Ok(())
    }

    #[test]
    fn high_priority_capacity_plan_skips_runtime_for_zero_entries() {
        let plan = HighPriorityCapacityPlan::from_entry_count(0, Some(non_zero(4)));

        assert_eq!(plan.entry_count(), 0);
        assert_eq!(plan.worker_count(), None);
    }

    #[test]
    fn high_priority_capacity_plan_uses_one_worker_for_one_entry() {
        let plan = HighPriorityCapacityPlan::from_entry_count(1, Some(non_zero(8)));

        assert_eq!(plan.entry_count(), 1);
        assert_eq!(plan.worker_count().map(NonZeroUsize::get), Some(1));
    }

    #[test]
    fn high_priority_capacity_plan_uses_entry_count_within_parallelism() {
        let plan = HighPriorityCapacityPlan::from_entry_count(3, Some(non_zero(8)));

        assert_eq!(plan.entry_count(), 3);
        assert_eq!(plan.worker_count().map(NonZeroUsize::get), Some(3));
    }

    #[test]
    fn high_priority_capacity_plan_caps_workers_by_parallelism() {
        let plan = HighPriorityCapacityPlan::from_entry_count(8, Some(non_zero(2)));

        assert_eq!(plan.entry_count(), 8);
        assert_eq!(plan.worker_count().map(NonZeroUsize::get), Some(2));
    }

    #[test]
    fn high_priority_capacity_plan_falls_back_to_one_worker_without_parallelism() {
        let plan = HighPriorityCapacityPlan::from_entry_count(4, None);

        assert_eq!(plan.entry_count(), 4);
        assert_eq!(plan.worker_count().map(NonZeroUsize::get), Some(1));
    }

    #[test]
    fn high_priority_capacity_plan_counts_only_declared_high_priority_entries() {
        let services = vec![
            test_service(1, &STANDARD_TEST_ENTRY),
            test_service(2, &HIGH_PRIORITY_TEST_ENTRY),
            test_service(3, &ISOLATED_TEST_ENTRY),
            test_service(4, &HIGH_PRIORITY_TEST_ENTRY),
        ];

        let plan = HighPriorityCapacityPlan::from_services(&services);

        assert_eq!(plan.entry_count(), 2);
        assert!(plan.worker_count().is_some());
    }

    #[test]
    fn builder_capacity_plan_uses_filtered_final_registry() {
        let daemon = ServiceDaemon::builder()
            .with_registry(
                Registry::builder()
                    .with_tag("__unit_high_priority_capacity_primary__")
                    .build(),
            )
            .build();

        assert_eq!(daemon.high_priority_capacity.entry_count(), 1);
        assert!(daemon.high_priority_capacity.worker_count().is_some());
    }

    #[test]
    fn builder_capacity_plan_merges_infra_tags_without_double_counting() {
        let daemon = ServiceDaemon::builder()
            .with_registry(
                Registry::builder()
                    .with_tag("__unit_high_priority_capacity_primary__")
                    .build(),
            )
            .with_infra_tags(&[
                "__unit_high_priority_capacity_primary__",
                "__unit_high_priority_capacity_infra__",
            ])
            .build();

        assert_eq!(daemon.high_priority_capacity.entry_count(), 2);
        assert!(daemon.high_priority_capacity.worker_count().is_some());
    }

    #[test]
    fn builder_capacity_plan_counts_high_priority_triggers_and_services_equally() {
        let daemon = ServiceDaemon::builder()
            .with_registry(
                Registry::builder()
                    .with_tag("__unit_high_priority_capacity_parity__")
                    .build(),
            )
            .build();

        assert_eq!(daemon.high_priority_capacity.entry_count(), 2);
        assert!(daemon.high_priority_capacity.worker_count().is_some());
    }

    #[test]
    fn validate_dependency_graph_accepts_linear_provider_chain() {
        let tid_a = TypeId::of::<u8>();
        let tid_b = TypeId::of::<u16>();

        let a = ProviderEntry {
            name: "A",
            module: "test",
            type_id: tid_a,
            params: leaked_params(vec![ServiceParam {
                name: "b",
                type_name: "B",
                type_id: tid_b,
            }]),
            eager: false,
            init: noop_init,
        };
        let b = ProviderEntry {
            name: "B",
            module: "test",
            type_id: tid_b,
            params: &[],
            eager: false,
            init: noop_init,
        };

        validate_dependency_graph(&[], [&a, &b]).expect("linear chain must validate");
    }

    #[cfg(feature = "diagnostics")]
    #[test]
    fn shutdown_topology_is_emitted_as_tracing_event() {
        crate::core::topology_collector::reset_topology();
        crate::core::topology_collector::record_topology_edge_for_test(
            ServiceId::new(0),
            ServiceId::new(1),
        );

        let capture = CapturedTraceFields::default();
        let events = capture.events.clone();
        let subscriber = tracing_subscriber::registry().with(capture);

        tracing::subscriber::with_default(subscriber, emit_shutdown_topology);

        let events = events
            .lock()
            .unwrap_or_else(|err| panic!("trace capture lock poisoned: {err}"))
            .clone();
        let topology_event = events
            .iter()
            .find(|event| event.get("topology_mermaid").is_some())
            .unwrap_or_else(|| panic!("expected topology_mermaid event, got: {events:?}"));
        let mermaid = topology_event
            .get("topology_mermaid")
            .expect("topology event should carry Mermaid text");

        assert!(
            mermaid.contains("graph LR"),
            "Mermaid topology should be carried as a tracing field, got: {mermaid}"
        );
        assert_eq!(
            topology_event.get("message").map(String::as_str),
            Some("behavioral topology exported during shutdown")
        );

        crate::core::topology_collector::reset_topology();
    }

    #[test]
    fn validate_dependency_graph_rejects_provider_cycle() {
        let tid_a = TypeId::of::<u8>();
        let tid_b = TypeId::of::<u16>();

        // A depends on B and B depends on A -- classic two-node cycle.
        let a = ProviderEntry {
            name: "A",
            module: "test",
            type_id: tid_a,
            params: leaked_params(vec![ServiceParam {
                name: "b",
                type_name: "B",
                type_id: tid_b,
            }]),
            eager: false,
            init: noop_init,
        };
        let b = ProviderEntry {
            name: "B",
            module: "test",
            type_id: tid_b,
            params: leaked_params(vec![ServiceParam {
                name: "a",
                type_name: "A",
                type_id: tid_a,
            }]),
            eager: false,
            init: noop_init,
        };

        let err =
            validate_dependency_graph(&[], [&a, &b]).expect_err("cycle must surface as an error");
        match err {
            ProviderInitError::Fatal { provider, message } => {
                assert!(
                    matches!(provider.as_str(), "A" | "B"),
                    "unexpected offending provider: {provider}"
                );
                assert!(
                    message.contains("Circular"),
                    "message should mention circularity, got: {message}"
                );
            }
            other => panic!("expected ProviderInitError::Fatal, got {other:?}"),
        }
    }

    fn setup_tracing() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    #[tokio::test]
    async fn test_service_daemon_builder_default() {
        setup_tracing();
        let daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .build();
        debug!("test_service_daemon_builder_default passed");
        let _ = daemon;
    }

    #[test]
    fn build_does_not_create_high_priority_runtime() {
        let daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .build();

        assert!(daemon.high_priority_runtime.is_none());
    }

    #[test]
    fn ensure_high_priority_runtime_skips_standard_only_services() {
        let mut daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .build();
        daemon.services = vec![test_service(1, &STANDARD_TEST_ENTRY)];
        daemon.high_priority_capacity = HighPriorityCapacityPlan::from_services(&daemon.services);

        let runtime = daemon
            .ensure_high_priority_runtime()
            .expect("runtime check should not fail for standard-only services");

        assert!(runtime.is_none());
        assert!(daemon.high_priority_runtime.is_none());
    }

    #[test]
    fn ensure_high_priority_runtime_creates_for_high_priority_services() {
        let mut daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .build();
        daemon.services = vec![test_service(1, &HIGH_PRIORITY_TEST_ENTRY)];
        daemon.high_priority_capacity = HighPriorityCapacityPlan::from_services(&daemon.services);

        let runtime = daemon
            .ensure_high_priority_runtime()
            .expect("runtime creation should succeed for high-priority services");

        assert!(runtime.is_some());
        assert!(daemon.high_priority_runtime.is_some());
        daemon.shutdown_high_priority_runtime();
        assert!(daemon.high_priority_runtime.is_none());
    }

    #[tokio::test]
    async fn run_keeps_high_priority_runtime_absent_for_empty_registry() {
        let mut daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .build();

        daemon.run().await;

        assert!(daemon.high_priority_runtime.is_none());
    }

    #[test]
    fn shutdown_high_priority_runtime_is_noop_when_never_created() {
        let mut daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .build();

        daemon.shutdown_high_priority_runtime();

        assert!(daemon.high_priority_runtime.is_none());
    }

    #[tokio::test]
    async fn do_shutdown_drops_high_priority_runtime_before_control_runtime() {
        let drop_order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let high_priority_drop_order = drop_order.clone();
        let control_drop_order = drop_order.clone();
        let mut daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .build();

        daemon.high_priority_runtime = Some(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(1)
                .thread_name("test-high-priority-drop")
                .on_thread_stop(move || {
                    high_priority_drop_order
                        .lock()
                        .expect("drop order mutex should not be poisoned")
                        .push("high_priority");
                })
                .build()
                .expect("high-priority runtime should build"),
        );
        daemon.control_runtime = Some(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(1)
                .thread_name("test-control-drop")
                .on_thread_stop(move || {
                    control_drop_order
                        .lock()
                        .expect("drop order mutex should not be poisoned")
                        .push("control");
                })
                .build()
                .expect("control runtime should build"),
        );

        daemon.do_shutdown().await;

        assert!(daemon.high_priority_runtime.is_none());
        assert!(daemon.control_runtime.is_none());
        assert_eq!(
            drop_order
                .lock()
                .expect("drop order mutex should not be poisoned")
                .as_slice(),
            ["high_priority", "control"]
        );
    }

    #[tokio::test]
    async fn test_service_daemon_handle() {
        setup_tracing();
        let daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .build();
        let handle = daemon.handle();

        // Initially, unknown service should be Terminated
        let status = handle.get_service_status(&ServiceId(999)).await;
        assert_eq!(status, ServiceStatus::Terminated);

        // Insert a status manually and verify
        daemon
            .resources
            .status_plane
            .insert(ServiceId(1), ServiceStatus::Healthy);
        let status = handle.get_service_status(&ServiceId(1)).await;
        assert_eq!(status, ServiceStatus::Healthy);
    }

    #[tokio::test]
    async fn test_service_status_update() {
        setup_tracing();
        let daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .build();
        let handle = daemon.handle();

        // Insert status
        daemon
            .resources
            .status_plane
            .insert(ServiceId(0), ServiceStatus::Initializing);

        let status = handle.get_service_status(&ServiceId(0)).await;
        assert_eq!(status, ServiceStatus::Initializing);

        // Update status
        daemon
            .resources
            .status_plane
            .insert(ServiceId(0), ServiceStatus::Healthy);
        let status = handle.get_service_status(&ServiceId(0)).await;
        assert_eq!(status, ServiceStatus::Healthy);
    }

    #[test]
    fn runtime_snapshots_are_available_from_daemon_and_handle() {
        setup_tracing();
        let mut daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .build();
        daemon.services = vec![
            test_service(2, &HIGH_PRIORITY_TEST_ENTRY),
            test_service(1, &STANDARD_TEST_ENTRY),
        ];
        daemon
            .resources
            .runtime_facts
            .register_services(&daemon.services);
        daemon
            .resources
            .status_plane
            .insert(ServiceId::new(1), ServiceStatus::Healthy);
        daemon.resources.status_plane.insert(
            ServiceId::new(2),
            ServiceStatus::Recovering("temporary failure".to_owned()),
        );

        let daemon_runtime = daemon.runtime();
        let handle = daemon.handle();
        let handle_runtime = handle.runtime();
        assert_eq!(daemon_runtime.daemon_id, handle_runtime.daemon_id);
        assert_eq!(handle_runtime.service_count, 2);
        assert!(!handle_runtime.shutdown_requested);

        let services = handle.runtime_services();
        assert_eq!(
            services
                .iter()
                .map(|snapshot| snapshot.service_id)
                .collect::<Vec<_>>(),
            vec![ServiceId::new(1), ServiceId::new(2)]
        );
        assert_eq!(
            handle
                .runtime_service(ServiceId::new(1))
                .map(|snapshot| snapshot.status),
            Some(ServiceStatus::Healthy)
        );
        assert!(handle.runtime_service(ServiceId::new(999)).is_none());

        let readiness = handle.runtime_readiness();
        assert_eq!(readiness.healthy.len(), 1);
        assert_eq!(readiness.recovering.len(), 1);
        assert_eq!(readiness.recent_errors.len(), 1);
        assert_eq!(readiness.recent_errors[0].message, "temporary failure");
    }

    #[test]
    fn runtime_snapshot_reports_shutdown_requested() {
        setup_tracing();
        let daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .build();
        let handle = daemon.handle();

        assert!(!handle.runtime().shutdown_requested);
        daemon.shutdown();
        assert!(handle.runtime().shutdown_requested);
    }

    #[test]
    fn diagnostics_snapshot_is_available_from_daemon_and_handle() {
        setup_tracing();
        let daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .build();
        let handle = daemon.handle();

        let daemon_snapshot = daemon.diagnostics_snapshot();
        let handle_snapshot = handle.diagnostics_snapshot();

        assert_eq!(daemon_snapshot, handle_snapshot);
        assert_eq!(daemon_snapshot.services.len(), 0);
        assert_eq!(daemon_snapshot.generations.len(), 0);
        assert!(
            daemon_snapshot
                .lanes
                .iter()
                .any(|lane| { lane.runtime_lane == crate::models::DiagnosticRuntimeLane::Control })
        );
    }

    #[test]
    fn default_advisory_profile_spawns_recommendation_loop() {
        setup_tracing();
        let mut daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .build();
        let control_runtime = daemon
            .ensure_control_runtime()
            .expect("control runtime should build");

        daemon.spawn_adaptive_recommendation_loop(&control_runtime);

        assert!(daemon.adaptive_recommendation_task.is_some());
        daemon.shutdown();
    }

    #[test]
    fn disabled_advisory_profile_skips_recommendation_loop() {
        setup_tracing();
        let mut daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .with_scheduling_advisory_profile(SchedulingAdvisoryProfile::disabled())
            .build();
        let control_runtime = daemon
            .ensure_control_runtime()
            .expect("control runtime should build");

        daemon.spawn_adaptive_recommendation_loop(&control_runtime);

        assert!(daemon.adaptive_recommendation_task.is_none());
    }

    #[test]
    fn default_isolated_startup_limit_uses_internal_default() {
        let daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .build();

        assert_eq!(
            daemon.isolated_startup_permits.available_permits(),
            ISOLATED_STARTUP_CONCURRENCY_LIMIT
        );
    }

    #[test]
    fn builder_configures_isolated_startup_limit() {
        let limit = NonZeroUsize::new(2).expect("test limit should be non-zero");
        let daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .with_isolated_startup_concurrency_limit(limit)
            .build();

        assert_eq!(daemon.isolated_startup_permits.available_permits(), 2);
    }

    /// Global counter for the `counting_service` test service.
    static SHORT_RUN_COUNT: AtomicU32 = AtomicU32::new(0);

    /// Test service that increments a global counter on each invocation.
    #[service(tags = ["__test_short_run__"], priority = 50)]
    async fn counting_service() -> anyhow::Result<()> {
        SHORT_RUN_COUNT.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok(())
    }

    #[cfg(feature = "simulation")]
    #[tokio::test]
    async fn test_short_run() {
        setup_tracing();
        SHORT_RUN_COUNT.store(0, Ordering::SeqCst);

        let daemon = ServiceDaemon::builder()
            .with_registry(Registry::builder().with_tag("__test_short_run__").build())
            .with_restart_policy(RestartPolicy::for_testing())
            .build();

        let start = Instant::now();
        daemon
            .run_for_duration(Duration::from_millis(500))
            .await
            .unwrap();
        let elapsed = start.elapsed();

        // Should have run and restarted a few times
        let count = SHORT_RUN_COUNT.load(Ordering::SeqCst);
        assert!(
            count >= 1,
            "Service should have run at least once, got {}",
            count
        );
        assert!(
            elapsed >= Duration::from_millis(400),
            "Should have run for at least 400ms"
        );
    }
}
