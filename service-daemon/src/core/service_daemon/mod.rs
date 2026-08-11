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

#[cfg(feature = "simulation")]
use std::any::Any;
use std::collections::HashMap;
use std::future::pending;
use std::sync::{Arc, OnceLock};
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
#[cfg(unix)]
use crate::models::ServiceError;
use crate::models::{
    DaemonDiagnosticsSnapshot, DaemonInstanceId, DaemonRuntimeSnapshot, ReadinessSnapshot,
    Result as ServiceResult, SchedulingAdvisoryProfile, ServiceDescription, ServiceHandle,
    ServiceInstanceHandle, ServiceInstanceId, ServiceInstanceRegistry, ServiceRuntimeSnapshot,
    ServiceStatus, TriggerRuntimeSnapshot,
};
use dashmap::DashMap;

pub use builder::ServiceDaemonBuilder;
pub use policy::{RestartPolicy, RestartPolicyBuilder};
use runtime::HighPriorityCapacityPlan;
use startup_pipeline::StartupError;

// ---------------------------------------------------------------------------
// DaemonInstanceHandle -- daemon instance control interface
// ---------------------------------------------------------------------------

/// A handle to one daemon instance owned by the process-local daemon registry.
#[derive(Clone)]
pub struct DaemonInstanceHandle {
    id: DaemonInstanceId,
    inner: Arc<Mutex<DaemonInstanceInner>>,
    resources: Arc<DaemonResources>,
    diagnostics: Arc<DiagnosticsStore>,
    instance_registry: Arc<ServiceInstanceRegistry>,
    shutdown_token: CancellationToken,
    external_cancel_token: Option<CancellationToken>,
}

impl DaemonInstanceHandle {
    /// Return this daemon instance identity.
    pub fn id(&self) -> DaemonInstanceId {
        self.id
    }

    /// Get the cancellation token for this daemon.
    pub fn cancel_token(&self) -> CancellationToken {
        self.shutdown_token.clone()
    }

    /// Get the current status of a service instance handle.
    pub async fn get_instance_status(&self, handle: &ServiceInstanceHandle) -> ServiceStatus {
        if !self.owns_instance(handle) {
            return ServiceStatus::Terminated;
        }
        self.status_for_instance_id(handle.instance_id())
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
            .readiness_snapshot(|service_instance_id| self.status_for_snapshot(service_instance_id))
    }

    /// Return read-only runtime facts for all registered services.
    pub fn runtime_services(&self) -> Vec<ServiceRuntimeSnapshot> {
        self.resources
            .runtime_facts
            .service_snapshots(|service_instance_id| self.status_for_snapshot(service_instance_id))
    }

    /// Return read-only runtime facts for a service instance handle.
    pub fn runtime_instance(
        &self,
        handle: &ServiceInstanceHandle,
    ) -> Option<ServiceRuntimeSnapshot> {
        if !self.owns_instance(handle) {
            return None;
        }
        self.runtime_snapshot_for_instance_id(handle.instance_id())
    }

    /// Request shutdown for one managed service instance.
    ///
    /// This cancels the instance token and updates status/runtime facts. It
    /// does not remove the instance from the daemon registry and does not wait
    /// for the task join handle to finish.
    pub fn request_stop_instance(&self, handle: &ServiceInstanceHandle) -> bool {
        let Some(record) = self.instance_registry.get(handle.instance_id()) else {
            return false;
        };
        if record.handle() != *handle {
            return false;
        }

        record.cancellation_token().cancel();
        let shutting_down = ServiceStatus::ShuttingDown;
        self.resources
            .status_plane
            .insert(handle.instance_id(), shutting_down.clone());
        self.resources
            .runtime_facts
            .record_service_status(handle.instance_id(), &shutting_down);
        self.resources.status_changed.notify_waiters();
        true
    }

    /// Return read-only runtime facts for all observed triggers.
    pub fn runtime_triggers(&self) -> Vec<TriggerRuntimeSnapshot> {
        self.resources.runtime_facts.trigger_snapshots()
    }

    /// Return read-only runtime facts for an observed trigger service instance.
    pub fn runtime_trigger_instance(
        &self,
        handle: &ServiceInstanceHandle,
    ) -> Option<TriggerRuntimeSnapshot> {
        if !self.owns_instance(handle) {
            return None;
        }
        self.resources
            .runtime_facts
            .trigger_snapshot(handle.instance_id())
    }

    /// Return all service instances owned by this daemon.
    pub fn service_instances(&self) -> Vec<ServiceInstanceHandle> {
        self.instance_registry
            .records()
            .into_iter()
            .map(|record| record.handle())
            .collect()
    }

    /// Return service instances for one service definition selected by this daemon.
    pub fn service_instances_for(&self, handle: &ServiceHandle) -> Vec<ServiceInstanceHandle> {
        let Some(projection) = self.resources.service_catalog_projection() else {
            return Vec::new();
        };
        if projection.resolve_handle(handle.entry_id()) != Some(*handle) {
            return Vec::new();
        }
        self.instance_registry.handles_for_entry(handle.entry_id())
    }

    /// Start the daemon in the background (non-blocking).
    #[instrument(skip(self))]
    pub async fn run(&self) {
        let mut inner = self.inner.lock().await;
        inner.run().await;
    }

    /// Wait for the daemon to stop and unregister it from the process-local registry.
    #[instrument(skip(self))]
    pub async fn wait(&self) -> ServiceResult<()> {
        let result = {
            let mut inner = self.inner.lock().await;
            inner.wait().await
        };
        daemon_registry().unregister(self.id);
        result
    }

    /// Trigger graceful shutdown of the daemon.
    pub fn shutdown(&self) {
        info!("DaemonInstanceHandle::shutdown() called, triggering graceful termination...");
        self.shutdown_token.cancel();
        if let Some(ref external) = self.external_cancel_token {
            external.cancel();
        }
    }

    /// Run for a limited duration using simulation startup semantics.
    #[cfg(feature = "simulation")]
    #[instrument(skip(self))]
    pub(crate) async fn simulation_run_for_duration(
        &self,
        duration: Duration,
    ) -> ServiceResult<()> {
        let result = {
            let mut inner = self.inner.lock().await;
            inner.run_for_duration(duration).await
        };
        daemon_registry().unregister(self.id);
        result
    }

    #[cfg(feature = "simulation")]
    pub(crate) fn simulation_set_shelf<T: Any + Send + Sync>(
        &self,
        service_instance_id: ServiceInstanceId,
        key: &str,
        value: T,
    ) {
        let entry = self.resources.shelf.entry(service_instance_id).or_default();
        entry.insert(key.to_string(), Box::new(value));
    }

    #[cfg(feature = "simulation")]
    pub(crate) fn simulation_set_status(
        &self,
        service_instance_id: ServiceInstanceId,
        status: ServiceStatus,
    ) {
        self.resources
            .status_plane
            .insert(service_instance_id, status);
        self.resources.status_changed.notify_waiters();
    }

    #[cfg(feature = "simulation")]
    pub(crate) fn simulation_trigger_reload(&self, service_instance_id: &ServiceInstanceId) {
        if let Some(notify) = self.resources.reload_signals.get(service_instance_id) {
            notify.notify_one();
        }
    }

    #[cfg(feature = "simulation")]
    pub(crate) fn simulation_override_provider<T>(&self, value: T)
    where
        T: 'static + Send + Sync + Clone,
    {
        self.resources
            .provider_scope
            .override_local_slot(Arc::new(value));
    }

    #[cfg(feature = "simulation")]
    pub(crate) fn simulation_service_instance_ids(&self) -> Vec<ServiceInstanceId> {
        self.resources
            .status_plane
            .iter()
            .map(|entry| *entry.key())
            .collect()
    }

    #[cfg(feature = "simulation")]
    pub(crate) fn simulation_get_shelf<T: Any + Clone + Send + Sync>(
        &self,
        service_instance_id: ServiceInstanceId,
        key: &str,
    ) -> Option<T> {
        self.resources
            .shelf
            .get(&service_instance_id)
            .and_then(|entry| {
                entry
                    .get(key)
                    .and_then(|val| val.downcast_ref::<T>().cloned())
            })
    }

    #[cfg(feature = "simulation")]
    pub(crate) fn simulation_get_status(
        &self,
        service_instance_id: ServiceInstanceId,
    ) -> Option<ServiceStatus> {
        self.resources
            .status_plane
            .get(&service_instance_id)
            .map(|status| status.value().clone())
    }

    #[cfg(feature = "simulation")]
    pub(crate) fn simulation_has_shelf(
        &self,
        service_instance_id: ServiceInstanceId,
        key: &str,
    ) -> bool {
        self.resources
            .shelf
            .get(&service_instance_id)
            .is_some_and(|entry| entry.contains_key(key))
    }

    #[cfg(feature = "simulation")]
    pub(crate) fn simulation_shelf_keys(
        &self,
        service_instance_id: ServiceInstanceId,
    ) -> Vec<String> {
        self.resources
            .shelf
            .get(&service_instance_id)
            .map(|entry| entry.iter().map(|kv| kv.key().clone()).collect())
            .unwrap_or_default()
    }

    fn status_for_snapshot(&self, id: ServiceInstanceId) -> ServiceStatus {
        self.status_for_instance_id(id)
    }

    fn status_for_instance_id(&self, id: ServiceInstanceId) -> ServiceStatus {
        self.resources
            .status_plane
            .get(&id)
            .map(|status| status.clone())
            .unwrap_or(ServiceStatus::Initializing)
    }

    fn runtime_snapshot_for_instance_id(
        &self,
        id: ServiceInstanceId,
    ) -> Option<ServiceRuntimeSnapshot> {
        self.resources
            .runtime_facts
            .service_snapshot(id, |service_instance_id| {
                self.status_for_snapshot(service_instance_id)
            })
    }

    fn owns_instance(&self, handle: &ServiceInstanceHandle) -> bool {
        self.instance_registry
            .get(handle.instance_id())
            .is_some_and(|record| record.handle() == *handle)
    }
}

struct DaemonRegistry {
    daemons: DashMap<DaemonInstanceId, Arc<Mutex<DaemonInstanceInner>>>,
}

impl DaemonRegistry {
    fn new() -> Self {
        Self {
            daemons: DashMap::new(),
        }
    }

    fn register(&self, inner: DaemonInstanceInner) -> DaemonInstanceHandle {
        let id = inner.resources.daemon_id();
        let resources = inner.resources.clone();
        let diagnostics = inner.diagnostics.clone();
        let instance_registry = inner.instance_registry.clone();
        let shutdown_token = inner.cancellation_token.clone();
        let external_cancel_token = inner.external_cancel_token.clone();
        let inner = Arc::new(Mutex::new(inner));
        self.daemons.insert(id, inner.clone());
        DaemonInstanceHandle {
            id,
            inner,
            resources,
            diagnostics,
            instance_registry,
            shutdown_token,
            external_cancel_token,
        }
    }

    fn unregister(&self, id: DaemonInstanceId) {
        self.daemons.remove(&id);
    }

    #[cfg(test)]
    fn contains(&self, id: DaemonInstanceId) -> bool {
        self.daemons.contains_key(&id)
    }
}

fn daemon_registry() -> &'static DaemonRegistry {
    static DAEMON_REGISTRY: OnceLock<DaemonRegistry> = OnceLock::new();
    DAEMON_REGISTRY.get_or_init(DaemonRegistry::new)
}

// ---------------------------------------------------------------------------
// ServiceDaemon -- Infallible Builder pattern
// ---------------------------------------------------------------------------

/// Lightweight facade for constructing daemon instances.
///
/// `ServiceDaemon::builder()` returns a [`ServiceDaemonBuilder`]. Building the
/// builder registers a process-local daemon instance and returns a
/// [`DaemonInstanceHandle`], which is the public control surface for running,
/// waiting, shutdown, and runtime snapshots.
///
/// # Examples
/// ```rust,ignore
/// // Non-blocking start, then wait for Ctrl+C:
/// let daemon = ServiceDaemon::builder().build();
/// daemon.run().await;
/// daemon.wait().await?;
///
/// // Hierarchical integration with external CancellationToken:
/// let root_token = CancellationToken::new();
/// let daemon = ServiceDaemon::builder()
///     .with_cancel_token(root_token.clone())
///     .build();
/// daemon.run().await;
/// // ... other work using root_token ...
/// daemon.wait().await?;
/// ```
pub struct ServiceDaemon;

impl ServiceDaemon {
    /// Start building a new daemon instance.
    #[must_use]
    pub fn builder() -> ServiceDaemonBuilder {
        ServiceDaemonBuilder::new()
    }
}

pub(crate) struct DaemonInstanceInner {
    services: Vec<ServiceDescription>,
    instance_registry: Arc<ServiceInstanceRegistry>,
    running_tasks: Arc<Mutex<HashMap<ServiceInstanceId, JoinHandle<()>>>>,
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

impl Drop for DaemonInstanceInner {
    fn drop(&mut self) {
        self.abort_adaptive_recommendation_loop();
        self.shutdown_high_priority_runtime_detached();
        self.shutdown_control_runtime_detached();
    }
}

impl DaemonInstanceInner {
    /// Start the daemon in the background (non-blocking).
    ///
    /// This method spawns all registered services using wave-based priorities
    /// and returns immediately. The daemon continues running in the background.
    ///
    /// Use [`wait()`](DaemonInstanceHandle::wait) to block until a shutdown signal,
    /// or [`shutdown()`](DaemonInstanceHandle::shutdown) to trigger graceful termination.
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
    /// - The internal cancellation token is cancelled (via [`shutdown()`](DaemonInstanceHandle::shutdown)).
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
    /// [`wait()`](DaemonInstanceHandle::wait) to proceed with the shutdown sequence.
    /// If an external token was provided, it is also cancelled to propagate
    /// the shutdown signal to other components sharing that token.
    pub fn shutdown(&self) {
        info!("Daemon instance shutdown requested, triggering graceful termination...");
        self.cancellation_token.cancel();
        // Propagate shutdown to external token if present
        if let Some(ref external) = self.external_cancel_token {
            external.cancel();
        }
    }

    /// Internal helper: perform the actual graceful shutdown sequence.
    async fn do_shutdown(&mut self) {
        if let Some(control_runtime) = self.control_runtime.as_ref() {
            let instances = self.instance_registry.records();
            let running_tasks = self.running_tasks.clone();
            let resources = self.resources.clone();
            let cancellation_token = self.cancellation_token.clone();
            let wave_stop_timeout = self.restart_policy.wave_stop_timeout;
            let shutdown = control_runtime.spawn(async move {
                runner::stop_all_services(
                    &instances,
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
            let instances = self.instance_registry.records();
            runner::stop_all_services(
                &instances,
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
    pub async fn run_for_duration(&mut self, duration: Duration) -> ServiceResult<()> {
        // Use testing policy with shorter delays
        let test_policy = RestartPolicy::for_testing();

        self.run_simulation_startup(test_policy).await?;

        tokio::time::sleep(duration).await;

        let instances = self.instance_registry.records();
        runner::stop_all_services(
            &instances,
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
        ProviderEntry, ProviderInitError, Registry, ServiceEntry, ServiceEntryId, ServiceHandle,
        ServiceInstanceHandle, ServiceInstanceRecord, ServiceInstanceRegistry, ServiceParam,
        ServiceScheduling,
    };
    use crate::{TT::*, provider, service, trigger};
    use std::any::TypeId;
    #[cfg(feature = "diagnostics")]
    use std::collections::BTreeMap;
    use std::num::NonZeroUsize;
    use std::sync::Arc;
    #[cfg(feature = "diagnostics")]
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicU32, Ordering};
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
        let entry_id = ServiceEntryId::new(id);
        let instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(id as u128));
        let instance_registry = Arc::new(ServiceInstanceRegistry::new());
        instance_registry.insert(ServiceInstanceRecord::new(
            ServiceInstanceHandle::new(instance_id, entry_id, entry),
            CancellationToken::new(),
        ));
        ServiceDescription {
            entry_id,
            entry,
            instance_registry,
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
        let daemon = test_inner_builder()
            .with_registry(
                Registry::builder()
                    .with_tag("__unit_high_priority_capacity_primary__")
                    .build(),
            )
            .build_inner();

        assert_eq!(daemon.high_priority_capacity.entry_count(), 1);
        assert!(daemon.high_priority_capacity.worker_count().is_some());
    }

    #[test]
    fn builder_capacity_plan_merges_infra_tags_without_double_counting() {
        let daemon = test_inner_builder()
            .with_registry(
                Registry::builder()
                    .with_tag("__unit_high_priority_capacity_primary__")
                    .build(),
            )
            .with_infra_tags(&[
                "__unit_high_priority_capacity_primary__",
                "__unit_high_priority_capacity_infra__",
            ])
            .build_inner();

        assert_eq!(daemon.high_priority_capacity.entry_count(), 2);
        assert!(daemon.high_priority_capacity.worker_count().is_some());
    }

    #[test]
    fn builder_infra_tag_merge_preserves_registry_entry_order() {
        let daemon = test_inner_builder()
            .with_registry(
                Registry::builder()
                    .with_tag("__unit_high_priority_capacity_primary__")
                    .build(),
            )
            .with_infra_tags(&["__unit_high_priority_capacity_infra__"])
            .build_inner();

        let entry_ids = daemon
            .services
            .iter()
            .map(|service| service.entry_id)
            .collect::<Vec<_>>();
        let mut sorted_entry_ids = entry_ids.clone();
        sorted_entry_ids.sort();

        assert_eq!(entry_ids, sorted_entry_ids);
    }

    #[test]
    fn builder_capacity_plan_counts_high_priority_triggers_and_services_equally() {
        let daemon = test_inner_builder()
            .with_registry(
                Registry::builder()
                    .with_tag("__unit_high_priority_capacity_parity__")
                    .build(),
            )
            .build_inner();

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
            ServiceInstanceId::new(uuid::Uuid::from_u128(0)),
            ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
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

    fn test_inner_builder() -> ServiceDaemonBuilder {
        ServiceDaemon::builder()
    }

    #[test]
    fn daemon_instance_id_display_and_parse_use_uuid_format() {
        let uuid = uuid::Uuid::from_u128(0x019fe6228039757196b706134213d2e7);
        let id = DaemonInstanceId::new(uuid);

        assert_eq!(
            id.to_string(),
            "daemon#019fe622-8039-7571-96b7-06134213d2e7"
        );
        assert_eq!(
            "daemon#019fe622-8039-7571-96b7-06134213d2e7"
                .parse::<DaemonInstanceId>()
                .expect("prefixed daemon id should parse"),
            id
        );
        assert_eq!(
            "019fe622-8039-7571-96b7-06134213d2e7"
                .parse::<DaemonInstanceId>()
                .expect("bare daemon UUID should parse"),
            id
        );
        assert_eq!(id.as_uuid(), uuid);
    }

    #[test]
    fn builder_registers_daemon_instance_and_runtime_snapshot_uses_same_id() {
        let daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .build();
        let daemon_id = daemon.id();

        assert!(daemon_registry().contains(daemon_id));
        assert_eq!(daemon.runtime().daemon_id, daemon_id);

        daemon_registry().unregister(daemon_id);
    }

    #[test]
    fn dropping_handle_clone_does_not_unregister_active_daemon() {
        let daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .build();
        let daemon_id = daemon.id();
        let clone = daemon.clone();

        drop(daemon);

        assert!(daemon_registry().contains(daemon_id));
        daemon_registry().unregister(clone.id());
    }

    #[tokio::test]
    async fn wait_unregisters_daemon_instance_after_terminal_cleanup() {
        let daemon = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .build();
        let daemon_id = daemon.id();

        daemon.shutdown();
        tokio::time::timeout(Duration::from_secs(1), daemon.wait())
            .await
            .expect("daemon wait should observe cancellation")
            .expect("daemon wait should complete cleanly");

        assert!(!daemon_registry().contains(daemon_id));
    }

    #[test]
    fn handle_lists_daemon_local_service_instances() {
        let registry = Registry::builder()
            .with_tag("__unit_high_priority_capacity_primary__")
            .build();
        let service = registry
            .services()
            .iter()
            .find(|service| service.name() == "capacity_primary_standard_service")
            .expect("standard test service should be selected by tag");
        let service_handle = ServiceHandle::new(service.entry_id, service.entry);
        let expected_instance = service
            .instances()
            .first()
            .copied()
            .expect("selected service should have one auto-start instance");
        let daemon = ServiceDaemon::builder().with_registry(registry).build();

        assert!(daemon.service_instances().contains(&expected_instance));
        assert_eq!(
            daemon.service_instances_for(&service_handle),
            vec![expected_instance]
        );

        daemon_registry().unregister(daemon.id());
    }

    #[tokio::test]
    async fn test_service_daemon_builder_default() {
        setup_tracing();
        let daemon = test_inner_builder()
            .with_registry(isolated_registry())
            .build_inner();
        debug!("test_service_daemon_builder_default passed");
        let _ = daemon;
    }

    #[test]
    fn build_does_not_create_high_priority_runtime() {
        let daemon = test_inner_builder()
            .with_registry(isolated_registry())
            .build_inner();

        assert!(daemon.high_priority_runtime.is_none());
    }

    #[test]
    fn ensure_high_priority_runtime_skips_standard_only_services() {
        let mut daemon = test_inner_builder()
            .with_registry(isolated_registry())
            .build_inner();
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
        let mut daemon = test_inner_builder()
            .with_registry(isolated_registry())
            .build_inner();
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
        let mut daemon = test_inner_builder()
            .with_registry(isolated_registry())
            .build_inner();

        daemon.run().await;

        assert!(daemon.high_priority_runtime.is_none());
    }

    #[test]
    fn shutdown_high_priority_runtime_is_noop_when_never_created() {
        let mut daemon = test_inner_builder()
            .with_registry(isolated_registry())
            .build_inner();

        daemon.shutdown_high_priority_runtime();

        assert!(daemon.high_priority_runtime.is_none());
    }

    #[tokio::test]
    async fn do_shutdown_drops_high_priority_runtime_before_control_runtime() {
        let drop_order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let high_priority_drop_order = drop_order.clone();
        let control_drop_order = drop_order.clone();
        let mut daemon = test_inner_builder()
            .with_registry(isolated_registry())
            .build_inner();

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
    async fn daemon_handle_returns_terminated_for_unknown_instance_handle() {
        setup_tracing();
        let handle = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .build();
        let unknown_instance = ServiceInstanceHandle::new(
            ServiceInstanceId::new(uuid::Uuid::from_u128(999)),
            ServiceEntryId::new(999),
            &STANDARD_TEST_ENTRY,
        );

        let status = handle.get_instance_status(&unknown_instance).await;
        assert_eq!(status, ServiceStatus::Terminated);
    }

    #[tokio::test]
    async fn daemon_handle_reads_status_by_instance_handle() {
        setup_tracing();
        let registry = Registry::builder()
            .with_tag("__unit_high_priority_capacity_primary__")
            .build();
        let instance = registry
            .services()
            .iter()
            .find(|service| service.name() == "capacity_primary_standard_service")
            .and_then(|service| service.instances().first().copied())
            .expect("standard test service should be selected by tag");
        let handle = ServiceDaemon::builder().with_registry(registry).build();

        let status = handle.get_instance_status(&instance).await;
        assert_eq!(status, ServiceStatus::Initializing);

        handle
            .resources
            .status_plane
            .insert(instance.instance_id(), ServiceStatus::Healthy);
        let status = handle.get_instance_status(&instance).await;
        assert_eq!(status, ServiceStatus::Healthy);
    }

    #[test]
    fn runtime_snapshots_are_available_from_daemon_and_handle() {
        setup_tracing();
        let mut daemon = test_inner_builder()
            .with_registry(isolated_registry())
            .build_inner();
        daemon.services = vec![
            test_service(2, &HIGH_PRIORITY_TEST_ENTRY),
            test_service(1, &STANDARD_TEST_ENTRY),
        ];
        let instance_records = daemon
            .services
            .iter()
            .flat_map(|service| service.instance_registry.records())
            .collect::<Vec<_>>();
        for record in &instance_records {
            daemon.instance_registry.insert(record.clone());
        }
        daemon
            .resources
            .runtime_facts
            .register_service_instances(&instance_records);
        daemon.resources.status_plane.insert(
            ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            ServiceStatus::Healthy,
        );
        daemon.resources.status_plane.insert(
            ServiceInstanceId::new(uuid::Uuid::from_u128(2)),
            ServiceStatus::Recovering("temporary failure".to_owned()),
        );

        let daemon_runtime = daemon
            .resources
            .runtime_facts
            .daemon_snapshot(daemon.cancellation_token.is_cancelled());
        let handle = daemon_registry().register(daemon);
        let handle_runtime = handle.runtime();
        assert_eq!(daemon_runtime.daemon_id, handle_runtime.daemon_id);
        assert_eq!(handle_runtime.service_count, 2);
        assert!(!handle_runtime.shutdown_requested);

        let services = handle.runtime_services();
        assert_eq!(
            services
                .iter()
                .map(|snapshot| snapshot.service_instance_id)
                .collect::<Vec<_>>(),
            vec![
                ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
                ServiceInstanceId::new(uuid::Uuid::from_u128(2))
            ]
        );
        let standard_instance = handle
            .service_instances()
            .into_iter()
            .find(|instance| {
                instance.instance_id() == ServiceInstanceId::new(uuid::Uuid::from_u128(1))
            })
            .expect("standard test service instance should be registered");
        assert_eq!(
            handle
                .runtime_instance(&standard_instance)
                .map(|snapshot| snapshot.status),
            Some(ServiceStatus::Healthy)
        );
        let fake_instance = ServiceInstanceHandle::new(
            ServiceInstanceId::new(uuid::Uuid::from_u128(999)),
            standard_instance.entry_id(),
            standard_instance.entry(),
        );
        assert!(handle.runtime_instance(&fake_instance).is_none());

        let readiness = handle.runtime_readiness();
        assert_eq!(readiness.healthy.len(), 1);
        assert_eq!(readiness.recovering.len(), 1);
        assert_eq!(readiness.recent_errors.len(), 1);
        assert_eq!(readiness.recent_errors[0].message, "temporary failure");
    }

    #[test]
    fn runtime_snapshot_reports_shutdown_requested() {
        setup_tracing();
        let handle = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .build();

        assert!(!handle.runtime().shutdown_requested);
        handle.shutdown();
        assert!(handle.runtime().shutdown_requested);
    }

    #[tokio::test]
    async fn request_stop_instance_updates_instance_status_and_runtime_facts() {
        setup_tracing();
        let registry = Registry::builder()
            .with_tag("__unit_high_priority_capacity_primary__")
            .build();
        let service = registry
            .services()
            .iter()
            .find(|service| service.name() == "capacity_primary_standard_service")
            .expect("standard test service should be selected by tag");
        let instance = service
            .instances()
            .first()
            .copied()
            .expect("auto-start service should have an instance handle");
        let fake_instance = ServiceInstanceHandle::new(
            ServiceInstanceId::new(uuid::Uuid::from_u128(999)),
            instance.entry_id(),
            instance.entry(),
        );
        let handle = ServiceDaemon::builder().with_registry(registry).build();

        assert!(!handle.request_stop_instance(&fake_instance));
        assert_eq!(
            handle.get_instance_status(&fake_instance).await,
            ServiceStatus::Terminated
        );

        assert!(handle.request_stop_instance(&instance));
        assert_eq!(
            handle.get_instance_status(&instance).await,
            ServiceStatus::ShuttingDown
        );
        assert_eq!(
            handle
                .runtime_instance(&instance)
                .map(|snapshot| snapshot.status),
            Some(ServiceStatus::ShuttingDown)
        );
    }

    #[test]
    fn diagnostics_snapshot_is_available_from_daemon_and_handle() {
        setup_tracing();
        let handle = ServiceDaemon::builder()
            .with_registry(isolated_registry())
            .build();

        let handle_snapshot = handle.diagnostics_snapshot();

        assert_eq!(handle_snapshot.services.len(), 0);
        assert_eq!(handle_snapshot.generations.len(), 0);
        assert!(
            handle_snapshot
                .lanes
                .iter()
                .any(|lane| { lane.runtime_lane == crate::models::DiagnosticRuntimeLane::Control })
        );
    }

    #[test]
    fn default_advisory_profile_spawns_recommendation_loop() {
        setup_tracing();
        let mut daemon = test_inner_builder()
            .with_registry(isolated_registry())
            .build_inner();
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
        let mut daemon = test_inner_builder()
            .with_registry(isolated_registry())
            .with_scheduling_advisory_profile(SchedulingAdvisoryProfile::disabled())
            .build_inner();
        let control_runtime = daemon
            .ensure_control_runtime()
            .expect("control runtime should build");

        daemon.spawn_adaptive_recommendation_loop(&control_runtime);

        assert!(daemon.adaptive_recommendation_task.is_none());
    }

    #[test]
    fn default_isolated_startup_limit_uses_internal_default() {
        let daemon = test_inner_builder()
            .with_registry(isolated_registry())
            .build_inner();

        assert_eq!(
            daemon.isolated_startup_permits.available_permits(),
            ISOLATED_STARTUP_CONCURRENCY_LIMIT
        );
    }

    #[test]
    fn builder_configures_isolated_startup_limit() {
        let limit = NonZeroUsize::new(2).expect("test limit should be non-zero");
        let daemon = test_inner_builder()
            .with_registry(isolated_registry())
            .with_isolated_startup_concurrency_limit(limit)
            .build_inner();

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

        let simulation = crate::MockContext::builder()
            .with_logging(false)
            .with_registry(Registry::builder().with_tag("__test_short_run__").build())
            .build();

        let start = Instant::now();
        simulation
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
