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
pub(crate) mod high_priority;
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
use std::any::TypeId;
use std::collections::HashMap;
use std::future::pending;
use std::sync::{
    Arc, OnceLock, Weak,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::runtime::{Handle, Runtime};
use tokio::sync::{Mutex, Notify, Semaphore, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument};

#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};

use crate::core::context::DaemonResources;
use crate::core::diagnostics::DiagnosticsStore;
use crate::models::ServiceError;
use crate::models::{
    DaemonDiagnosticsSnapshot, DaemonInstanceId, DaemonRuntimeSnapshot, ReadinessSnapshot,
    Result as ServiceResult, SchedulingAdvisoryProfile, ServiceControl, ServiceDescription,
    ServiceEntry, ServiceEntryId, ServiceHandle, ServiceInputPayload, ServiceInstanceHandle,
    ServiceInstanceId, ServiceInstanceRecord, ServiceInstanceRegistry, ServiceRuntimeSnapshot,
    ServiceScheduling, ServiceStatus, TriggerRuntimeSnapshot,
};
use dashmap::{DashMap, DashSet};
use high_priority::HighPriorityRuntimePool;

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
    control: Arc<DaemonInstanceControl>,
    diagnostics: Arc<DiagnosticsStore>,
    shutdown_token: CancellationToken,
    external_cancel_token: Option<CancellationToken>,
}

struct DaemonInstanceControl {
    id: DaemonInstanceId,
    resources: Arc<DaemonResources>,
    instance_registry: Arc<ServiceInstanceRegistry>,
    removing_instances: Arc<DashSet<ServiceInstanceId>>,
    stopping_instances: Arc<DashSet<ServiceInstanceId>>,
    startup_gate: Arc<StartupGate>,
    daemon_token: CancellationToken,
    inner: Weak<Mutex<DaemonInstanceInner>>,
}

#[derive(Clone)]
struct ServiceInstanceCleanupParts {
    instance_registry: Arc<ServiceInstanceRegistry>,
    running_tasks: Arc<Mutex<HashMap<ServiceInstanceId, JoinHandle<()>>>>,
    resources: Arc<DaemonResources>,
    diagnostics: Arc<DiagnosticsStore>,
    removing_instances: Arc<DashSet<ServiceInstanceId>>,
    stopping_instances: Arc<DashSet<ServiceInstanceId>>,
}

struct PreparedServiceStop {
    record: ServiceInstanceRecord,
    task: Option<JoinHandle<()>>,
    grace_period: Duration,
    control_runtime: Option<Handle>,
    resources: Arc<DaemonResources>,
    stopping_instances: Arc<DashSet<ServiceInstanceId>>,
}

struct PreparedServiceRemoval {
    record: ServiceInstanceRecord,
    task: Option<JoinHandle<()>>,
    grace_period: Duration,
    control_runtime: Option<Handle>,
    cleanup: ServiceInstanceCleanupParts,
}

#[derive(Default)]
struct StartupGate {
    started: AtomicBool,
    complete: AtomicBool,
    notify: Notify,
}

impl StartupGate {
    fn mark_started(&self) {
        self.started.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    fn mark_complete(&self) {
        self.complete.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    fn has_started(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }

    async fn wait_until_complete_or_cancelled(&self, cancellation_token: &CancellationToken) {
        loop {
            if self.complete.load(Ordering::Acquire) || cancellation_token.is_cancelled() {
                return;
            }
            tokio::select! {
                _ = self.notify.notified() => {}
                _ = cancellation_token.cancelled() => return,
            }
        }
    }
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

    /// Return a read-only snapshot of daemon diagnostics.
    pub fn diagnostics_snapshot(&self) -> DaemonDiagnosticsSnapshot {
        self.diagnostics.snapshot().into()
    }

    /// Return read-only daemon runtime facts.
    pub fn runtime(&self) -> DaemonRuntimeSnapshot {
        self.control
            .resources
            .runtime_facts
            .daemon_snapshot(self.shutdown_token.is_cancelled())
    }

    /// Return a facts-only readiness grouping.
    pub fn runtime_readiness(&self) -> ReadinessSnapshot {
        self.control
            .resources
            .runtime_facts
            .readiness_snapshot(|service_instance_id| self.status_for_snapshot(service_instance_id))
    }

    /// Return read-only runtime facts for all registered services.
    pub fn runtime_services(&self) -> Vec<ServiceRuntimeSnapshot> {
        self.control
            .resources
            .runtime_facts
            .service_snapshots(|service_instance_id| self.status_for_snapshot(service_instance_id))
    }

    /// Return read-only runtime facts for all observed triggers.
    pub fn runtime_triggers(&self) -> Vec<TriggerRuntimeSnapshot> {
        self.control.resources.runtime_facts.trigger_snapshots()
    }

    /// Return all service instances owned by this daemon.
    pub fn service_instances(&self) -> Vec<ServiceInstanceHandle> {
        self.control
            .instance_registry
            .records()
            .into_iter()
            .map(|record| ServiceInstanceHandle::from_record(&record, self.control.clone()))
            .collect()
    }

    /// Return service instances for one service definition selected by this daemon.
    pub fn service_instances_for(&self, handle: &ServiceHandle) -> Vec<ServiceInstanceHandle> {
        if handle.daemon_id() != self.id {
            return Vec::new();
        }
        if !self
            .control
            .owns_service_entry(handle.entry_id(), handle.entry())
        {
            return Vec::new();
        }
        handle.instances()
    }

    /// Start the daemon in the background (non-blocking).
    #[instrument(skip(self))]
    pub async fn run(&self) {
        let control_runtime = {
            let mut inner = self.inner.lock().await;
            inner.run().await;
            inner
                .control_runtime
                .as_ref()
                .map(|runtime| runtime.handle().clone())
        };
        if let Some(control_runtime) = control_runtime {
            self.inner
                .lock()
                .await
                .spawn_high_priority_policy_loop(&control_runtime, self.inner.clone());
        }
    }

    /// Wait for the daemon to stop and unregister it from the process-local registry.
    #[instrument(skip(self))]
    pub async fn wait(&self) -> ServiceResult<()> {
        wait_for_shutdown_signal(&self.shutdown_token, &self.external_cancel_token).await?;
        self.inner.lock().await.do_shutdown().await;
        daemon_registry().unregister(self.id);
        Ok(())
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
        handle: &ServiceInstanceHandle,
        key: &str,
        value: T,
    ) -> bool {
        if !self.control.owns_instance(handle) {
            return false;
        }
        let entry = self
            .control
            .resources
            .shelf
            .entry(handle.instance_id())
            .or_default();
        entry.insert(key.to_string(), Box::new(value));
        true
    }

    #[cfg(feature = "simulation")]
    pub(crate) fn simulation_set_status(
        &self,
        handle: &ServiceInstanceHandle,
        status: ServiceStatus,
    ) -> bool {
        if !self.control.owns_instance(handle) {
            return false;
        }
        self.control
            .resources
            .status_plane
            .insert(handle.instance_id(), status);
        self.control.resources.status_changed.notify_waiters();
        true
    }

    #[cfg(feature = "simulation")]
    pub(crate) fn simulation_trigger_reload(&self, handle: &ServiceInstanceHandle) -> bool {
        if !self.control.owns_instance(handle) {
            return false;
        }
        if let Some(notify) = self
            .control
            .resources
            .reload_signals
            .get(&handle.instance_id())
        {
            notify.notify_one();
            return true;
        }
        false
    }

    #[cfg(feature = "simulation")]
    pub(crate) fn simulation_override_provider<T>(&self, value: T)
    where
        T: 'static + Send + Sync + Clone,
    {
        self.control
            .resources
            .provider_scope
            .override_local_slot(Arc::new(value));
    }

    #[cfg(feature = "simulation")]
    pub(crate) fn simulation_get_shelf<T: Any + Clone + Send + Sync>(
        &self,
        handle: &ServiceInstanceHandle,
        key: &str,
    ) -> Option<T> {
        if !self.control.owns_instance(handle) {
            return None;
        }
        self.control
            .resources
            .shelf
            .get(&handle.instance_id())
            .and_then(|entry| {
                entry
                    .get(key)
                    .and_then(|val| val.downcast_ref::<T>().cloned())
            })
    }

    #[cfg(feature = "simulation")]
    pub(crate) fn simulation_get_status(
        &self,
        handle: &ServiceInstanceHandle,
    ) -> Option<ServiceStatus> {
        if !self.control.owns_instance(handle) {
            return None;
        }
        self.control
            .resources
            .status_plane
            .get(&handle.instance_id())
            .map(|status| status.value().clone())
    }

    #[cfg(feature = "simulation")]
    pub(crate) fn simulation_has_shelf(&self, handle: &ServiceInstanceHandle, key: &str) -> bool {
        if !self.control.owns_instance(handle) {
            return false;
        }
        self.control
            .resources
            .shelf
            .get(&handle.instance_id())
            .is_some_and(|entry| entry.contains_key(key))
    }

    #[cfg(feature = "simulation")]
    pub(crate) fn simulation_shelf_keys(&self, handle: &ServiceInstanceHandle) -> Vec<String> {
        if !self.control.owns_instance(handle) {
            return Vec::new();
        }
        self.control
            .resources
            .shelf
            .get(&handle.instance_id())
            .map(|entry| entry.iter().map(|kv| kv.key().clone()).collect())
            .unwrap_or_default()
    }

    fn status_for_snapshot(&self, id: ServiceInstanceId) -> ServiceStatus {
        self.status_for_instance_id(id)
    }

    fn status_for_instance_id(&self, id: ServiceInstanceId) -> ServiceStatus {
        self.control
            .resources
            .status_plane
            .get(&id)
            .map(|status| status.clone())
            .unwrap_or(ServiceStatus::Initializing)
    }
}

impl ServiceControl for DaemonInstanceControl {
    fn daemon_id(&self) -> DaemonInstanceId {
        self.id
    }

    fn owns_service_entry(&self, entry_id: ServiceEntryId, entry: &'static ServiceEntry) -> bool {
        let Some(projection) = self.resources.service_catalog_projection() else {
            return false;
        };
        projection
            .resolve_entry(entry_id)
            .is_some_and(|record| std::ptr::eq(record.entry, entry))
    }

    fn service_instances_for_entry(
        &self,
        entry_id: ServiceEntryId,
        entry: &'static ServiceEntry,
        control: Arc<dyn ServiceControl>,
    ) -> Vec<ServiceInstanceHandle> {
        if !self.owns_service_entry(entry_id, entry) {
            return Vec::new();
        }
        self.instance_registry.handles_for_entry(entry_id, control)
    }

    fn service_status(&self, handle: &ServiceInstanceHandle) -> ServiceStatus {
        if !self.owns_instance(handle) {
            return ServiceStatus::Terminated;
        }
        self.status_for_instance_id(handle.instance_id())
    }

    fn service_runtime(&self, handle: &ServiceInstanceHandle) -> Option<ServiceRuntimeSnapshot> {
        if !self.owns_instance(handle) {
            return None;
        }
        self.runtime_snapshot_for_instance_id(handle.instance_id())
    }

    fn trigger_runtime(&self, handle: &ServiceInstanceHandle) -> Option<TriggerRuntimeSnapshot> {
        if !self.owns_instance(handle) {
            return None;
        }
        self.resources
            .runtime_facts
            .trigger_snapshot(handle.instance_id())
    }

    fn request_stop(&self, handle: &ServiceInstanceHandle) -> bool {
        if self.is_lifecycle_operation_pending(handle.instance_id()) {
            return false;
        }
        let Some(record) = self.instance_registry.get(handle.instance_id()) else {
            return false;
        };
        if !self.record_matches_handle(&record, handle) {
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

    fn create_service_instance(
        &self,
        handle: &ServiceHandle,
        input: Option<ServiceInputPayload>,
        actual_input_type_name: &'static str,
        actual_input_type_id: TypeId,
        control: Arc<dyn ServiceControl>,
    ) -> futures::future::BoxFuture<'static, ServiceResult<ServiceInstanceHandle>> {
        let entry_id = handle.entry_id();
        let entry = handle.entry();
        let inner = self.inner.clone();
        let startup_gate = self.startup_gate.clone();
        let daemon_token = self.daemon_token.clone();
        Box::pin(async move {
            if !startup_gate.has_started() {
                return Err(ServiceError::RegistryError(
                    "cannot create service instance before daemon run() starts".to_owned(),
                ));
            }
            startup_gate
                .wait_until_complete_or_cancelled(&daemon_token)
                .await;
            let Some(inner) = inner.upgrade() else {
                return Err(ServiceError::RegistryError(
                    "service handle owner daemon is no longer active".to_owned(),
                ));
            };
            let mut inner = inner.lock().await;
            inner
                .create_service_instance(
                    entry_id,
                    entry,
                    input,
                    actual_input_type_name,
                    actual_input_type_id,
                    control.clone(),
                )
                .await
        })
    }

    fn start_service_instance(
        &self,
        handle: &ServiceInstanceHandle,
    ) -> futures::future::BoxFuture<'static, ServiceResult<bool>> {
        let instance = handle.clone();
        let inner = self.inner.clone();
        let removing_instances = self.removing_instances.clone();
        let stopping_instances = self.stopping_instances.clone();
        Box::pin(async move {
            if removing_instances.contains(&instance.instance_id())
                || stopping_instances.contains(&instance.instance_id())
            {
                return Ok(false);
            }
            let Some(inner) = inner.upgrade() else {
                return Ok(false);
            };
            inner.lock().await.start_service_instance(&instance).await
        })
    }

    fn stop_service_instance(
        &self,
        handle: &ServiceInstanceHandle,
    ) -> futures::future::BoxFuture<'static, ServiceResult<bool>> {
        let instance = handle.clone();
        let inner = self.inner.clone();
        let removing_instances = self.removing_instances.clone();
        let stopping_instances = self.stopping_instances.clone();
        Box::pin(async move {
            if removing_instances.contains(&instance.instance_id())
                || stopping_instances.contains(&instance.instance_id())
            {
                return Ok(false);
            }
            let Some(inner) = inner.upgrade() else {
                return Ok(false);
            };
            let prepared = {
                let mut inner = inner.lock().await;
                inner.prepare_stop_service_instance(&instance).await?
            };
            let Some(prepared) = prepared else {
                return Ok(false);
            };
            let (tx, rx) = oneshot::channel();
            let control_runtime = prepared.control_runtime.clone();
            let task = async move {
                let result = finish_graceful_service_stop(prepared).await;
                let _ = tx.send(result);
            };
            if let Some(runtime) = control_runtime {
                runtime.spawn(task);
            } else {
                tokio::spawn(task);
            }
            rx.await.unwrap_or(Ok(false))
        })
    }

    fn remove_service_instance(
        &self,
        handle: &ServiceInstanceHandle,
    ) -> futures::future::BoxFuture<'static, ServiceResult<bool>> {
        let instance = handle.clone();
        let inner = self.inner.clone();
        let removing_instances = self.removing_instances.clone();
        let stopping_instances = self.stopping_instances.clone();
        Box::pin(async move {
            if removing_instances.contains(&instance.instance_id())
                || stopping_instances.contains(&instance.instance_id())
            {
                return Ok(false);
            }
            let Some(inner) = inner.upgrade() else {
                return Ok(false);
            };
            let prepared = {
                let mut inner = inner.lock().await;
                inner.prepare_remove_service_instance(&instance).await?
            };
            let Some(prepared) = prepared else {
                return Ok(false);
            };
            let (tx, rx) = oneshot::channel();
            let control_runtime = prepared.control_runtime.clone();
            let task = async move {
                let result = finish_graceful_service_removal(prepared).await;
                let _ = tx.send(result);
            };
            if let Some(runtime) = control_runtime {
                runtime.spawn(task);
            } else {
                tokio::spawn(task);
            }
            rx.await.unwrap_or(Ok(false))
        })
    }

    fn force_remove_service_instance(
        &self,
        handle: &ServiceInstanceHandle,
    ) -> futures::future::BoxFuture<'static, ServiceResult<bool>> {
        let instance = handle.clone();
        let inner = self.inner.clone();
        let removing_instances = self.removing_instances.clone();
        let stopping_instances = self.stopping_instances.clone();
        Box::pin(async move {
            if removing_instances.contains(&instance.instance_id())
                || stopping_instances.contains(&instance.instance_id())
            {
                return Ok(false);
            }
            let Some(inner) = inner.upgrade() else {
                return Ok(false);
            };
            inner
                .lock()
                .await
                .force_remove_service_instance(&instance)
                .await
        })
    }
}

impl DaemonInstanceControl {
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
                self.status_for_instance_id(service_instance_id)
            })
    }

    fn owns_instance(&self, handle: &ServiceInstanceHandle) -> bool {
        if handle.daemon_id() != self.id {
            return false;
        }
        self.instance_registry
            .get(handle.instance_id())
            .is_some_and(|record| self.record_matches_handle(&record, handle))
    }

    fn is_lifecycle_operation_pending(&self, instance_id: ServiceInstanceId) -> bool {
        self.removing_instances.contains(&instance_id)
            || self.stopping_instances.contains(&instance_id)
    }

    fn record_matches_handle(
        &self,
        record: &crate::models::ServiceInstanceRecord,
        handle: &ServiceInstanceHandle,
    ) -> bool {
        record.instance_id() == handle.instance_id()
            && record.entry_id() == handle.entry_id()
            && std::ptr::eq(record.entry(), handle.entry())
    }
}

struct DaemonRegistryEntry {
    _inner: Arc<Mutex<DaemonInstanceInner>>,
    _control: Arc<DaemonInstanceControl>,
}

struct DaemonRegistry {
    daemons: DashMap<DaemonInstanceId, Arc<DaemonRegistryEntry>>,
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
        let removing_instances = inner.removing_instances.clone();
        let stopping_instances = inner.stopping_instances.clone();
        let shutdown_token = inner.cancellation_token.clone();
        let external_cancel_token = inner.external_cancel_token.clone();
        let startup_gate = inner.startup_gate.clone();
        let inner = Arc::new(Mutex::new(inner));
        let control = Arc::new(DaemonInstanceControl {
            id,
            resources: resources.clone(),
            instance_registry,
            removing_instances,
            stopping_instances,
            startup_gate,
            daemon_token: shutdown_token.clone(),
            inner: Arc::downgrade(&inner),
        });
        resources.set_service_control(control.clone());
        let entry = Arc::new(DaemonRegistryEntry {
            _inner: inner.clone(),
            _control: control.clone(),
        });
        self.daemons.insert(id, entry);
        DaemonInstanceHandle {
            id,
            inner,
            control,
            diagnostics,
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

async fn wait_for_shutdown_signal(
    cancellation_token: &CancellationToken,
    external_cancel_token: &Option<CancellationToken>,
) -> ServiceResult<()> {
    #[cfg(unix)]
    {
        let mut sigint = signal(SignalKind::interrupt())
            .map_err(|e| ServiceError::InternalError(format!("Failed to setup SIGINT: {}", e)))?;
        let mut sigterm = signal(SignalKind::terminate())
            .map_err(|e| ServiceError::InternalError(format!("Failed to setup SIGTERM: {}", e)))?;

        tokio::select! {
            _ = sigint.recv() => {
                info!("Received SIGINT, shutting down...");
            }
            _ = sigterm.recv() => {
                info!("Received SIGTERM, shutting down...");
            }
            _ = cancellation_token.cancelled() => {
                info!("Received internal cancellation signal, shutting down...");
            }
            _ = wait_external_token(external_cancel_token) => {
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
            _ = cancellation_token.cancelled() => {
                info!("Received internal cancellation signal, shutting down...");
            }
            _ = wait_external_token(external_cancel_token) => {
                info!("Received external cancellation signal, shutting down...");
            }
        }
    }

    Ok(())
}

async fn wait_external_token(token: &Option<CancellationToken>) {
    match token {
        Some(t) => t.cancelled().await,
        None => pending().await,
    }
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
    removing_instances: Arc<DashSet<ServiceInstanceId>>,
    stopping_instances: Arc<DashSet<ServiceInstanceId>>,
    running_tasks: Arc<Mutex<HashMap<ServiceInstanceId, JoinHandle<()>>>>,
    restart_policy: RestartPolicy,
    cancellation_token: CancellationToken,
    /// Dedicated runtime for supervisor and control-plane work.
    control_runtime: Option<Runtime>,
    /// Handle for the runtime that hosts standard service bodies.
    standard_runtime: Option<Handle>,
    high_priority_capacity: HighPriorityCapacityPlan,
    /// Framework-owned runtime shards for HighPriority service bodies.
    high_priority_runtime_pool: HighPriorityRuntimePool,
    runtime_probe_tasks: Vec<JoinHandle<()>>,
    adaptive_recommendation_task: Option<JoinHandle<()>>,
    high_priority_policy_task: Option<JoinHandle<()>>,
    scheduling_advisory_profile: SchedulingAdvisoryProfile,
    /// Optional external token for hierarchical lifecycle management.
    /// When cancelled, the daemon treats it as a shutdown signal.
    external_cancel_token: Option<CancellationToken>,
    /// Instance-owned resources (Status Plane, Shelf, Signals)
    resources: Arc<DaemonResources>,
    diagnostics: Arc<DiagnosticsStore>,
    isolated_startup_permits: Arc<Semaphore>,
    startup_gate: Arc<StartupGate>,
}

impl Drop for DaemonInstanceInner {
    fn drop(&mut self) {
        self.abort_adaptive_recommendation_loop();
        self.abort_high_priority_policy_loop();
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
        self.startup_gate.mark_started();
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
            self.startup_gate.mark_complete();
            return self;
        }
        self.startup_gate.mark_complete();

        info!(
            "ServiceDaemon running with {} service(s).",
            self.services.len()
        );

        self
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
        self.stop_high_priority_policy_loop().await;
        self.stop_runtime_probes().await;
        self.standard_runtime = None;
        self.shutdown_high_priority_runtime();
        self.shutdown_control_runtime();

        #[cfg(feature = "diagnostics")]
        emit_shutdown_topology();

        info!("ServiceDaemon stopped.");
    }

    /// Run for a limited duration (for testing).
    #[cfg(feature = "simulation")]
    #[instrument(skip(self))]
    pub async fn run_for_duration(&mut self, duration: Duration) -> ServiceResult<()> {
        // Use testing policy with shorter delays
        let test_policy = RestartPolicy::for_testing();

        self.startup_gate.mark_started();
        self.run_simulation_startup(test_policy).await?;
        self.startup_gate.mark_complete();

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
        self.stop_high_priority_policy_loop().await;
        self.stop_runtime_probes().await;
        self.standard_runtime = None;
        self.shutdown_high_priority_runtime_detached();
        self.shutdown_control_runtime_detached();

        Ok(())
    }

    async fn create_service_instance(
        &mut self,
        entry_id: ServiceEntryId,
        entry: &'static ServiceEntry,
        input: Option<ServiceInputPayload>,
        actual_input_type_name: &'static str,
        actual_input_type_id: TypeId,
        control: Arc<dyn ServiceControl>,
    ) -> ServiceResult<ServiceInstanceHandle> {
        if self.cancellation_token.is_cancelled() {
            return Err(ServiceError::RegistryError(
                "cannot create service instance after daemon shutdown was requested".to_owned(),
            ));
        }
        if !self.owns_service_entry(entry_id, entry) {
            return Err(ServiceError::RegistryError(format!(
                "service entry {entry_id} is not selected by this daemon"
            )));
        }
        match entry.input {
            Some(expected) if actual_input_type_id != expected.type_id => {
                return Err(ServiceError::RegistryError(format!(
                    "service '{}' expects input '{}' of type '{}' but received '{}'",
                    entry.name, expected.name, expected.type_name, actual_input_type_name
                )));
            }
            Some(expected) if input.is_none() => {
                return Err(ServiceError::RegistryError(format!(
                    "service '{}' expects input '{}' of type '{}' but no input was provided",
                    entry.name, expected.name, expected.type_name
                )));
            }
            None if actual_input_type_id != TypeId::of::<()>() => {
                return Err(ServiceError::RegistryError(format!(
                    "service '{}' does not declare #[input] but received input type '{}'",
                    entry.name, actual_input_type_name
                )));
            }
            _ => {}
        }

        let record = ServiceInstanceRecord::with_input(
            ServiceInstanceId::new_v7(),
            entry_id,
            entry,
            CancellationToken::new(),
            input,
        );
        self.instance_registry.insert(record.clone());
        self.resources
            .runtime_facts
            .register_service_instances(std::slice::from_ref(&record));

        Ok(ServiceInstanceHandle::from_record(&record, control))
    }

    async fn start_service_instance(
        &mut self,
        handle: &ServiceInstanceHandle,
    ) -> ServiceResult<bool> {
        if self.cancellation_token.is_cancelled() {
            return Err(ServiceError::RegistryError(
                "cannot start service instance after daemon shutdown was requested".to_owned(),
            ));
        }
        let Some(record) = self.instance_registry.get(handle.instance_id()) else {
            return Ok(false);
        };
        if !record_matches_handle(&record, handle) {
            return Ok(false);
        }
        if self.is_lifecycle_operation_pending(handle.instance_id()) {
            return Ok(false);
        }
        if self
            .running_tasks
            .lock()
            .await
            .contains_key(&handle.instance_id())
        {
            return Ok(true);
        }

        let control_runtime = self
            .control_runtime
            .as_ref()
            .map(|runtime| runtime.handle().clone())
            .ok_or_else(|| {
                ServiceError::RegistryError(
                    "cannot start service instance before daemon run() prepares runtimes"
                        .to_owned(),
                )
            })?;
        let standard_runtime = self.standard_runtime.clone().ok_or_else(|| {
            ServiceError::RegistryError(
                "cannot start service instance before standard runtime is available".to_owned(),
            )
        })?;
        let high_priority_pool = (!self.high_priority_runtime_pool.is_empty())
            .then(|| self.high_priority_runtime_pool.state());
        if matches!(record.scheduling(), ServiceScheduling::HighPriority)
            && high_priority_pool.is_none()
        {
            return Err(ServiceError::RegistryError(format!(
                "HighPriority service '{}' is missing the shared high-priority runtime",
                record.name()
            )));
        }

        runner::spawn_service(parts::SpawnServiceParts {
            service_instance_id: record.instance_id(),
            name: record.name(),
            run: record.entry().wrapper,
            invocation_context: record.invocation_context(),
            watcher: record.entry().watcher,
            policy: self.restart_policy,
            scheduling: record.scheduling(),
            supervisor_lane: parts::SupervisorSpawnLane::Control(control_runtime),
            body_lanes: parts::BodyExecutionLanes {
                standard: standard_runtime,
                high_priority: high_priority_pool,
            },
            body_lane_resolver: parts::BodyLaneResolver::default(),
            running_tasks: self.running_tasks.clone(),
            resources: self.resources.clone(),
            diagnostics: self.diagnostics.clone(),
            isolated_startup_permits: self.isolated_startup_permits.clone(),
            cancellation_token: record.cancellation_token(),
            daemon_token: self.cancellation_token.clone(),
        })
        .await;

        Ok(true)
    }

    async fn prepare_stop_service_instance(
        &mut self,
        handle: &ServiceInstanceHandle,
    ) -> ServiceResult<Option<PreparedServiceStop>> {
        let Some(record) = self.instance_registry.get(handle.instance_id()) else {
            return Ok(None);
        };
        if !record_matches_handle(&record, handle) {
            return Ok(None);
        }
        if self.is_lifecycle_operation_pending(handle.instance_id()) {
            return Ok(None);
        }

        let mut running_tasks = self.running_tasks.lock().await;
        if !self.stopping_instances.insert(handle.instance_id()) {
            return Ok(None);
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

        let task = running_tasks.remove(&handle.instance_id());
        drop(running_tasks);

        Ok(Some(PreparedServiceStop {
            record,
            task,
            grace_period: self.restart_policy.wave_stop_timeout,
            control_runtime: self
                .control_runtime
                .as_ref()
                .map(|runtime| runtime.handle().clone()),
            resources: self.resources.clone(),
            stopping_instances: self.stopping_instances.clone(),
        }))
    }

    async fn prepare_remove_service_instance(
        &mut self,
        handle: &ServiceInstanceHandle,
    ) -> ServiceResult<Option<PreparedServiceRemoval>> {
        let Some(record) = self.instance_registry.get(handle.instance_id()) else {
            return Ok(None);
        };
        if !record_matches_handle(&record, handle) {
            return Ok(None);
        }
        if self.is_lifecycle_operation_pending(handle.instance_id()) {
            return Ok(None);
        }

        let mut running_tasks = self.running_tasks.lock().await;
        if !self.removing_instances.insert(handle.instance_id()) {
            return Ok(None);
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

        let task = running_tasks.remove(&handle.instance_id());
        drop(running_tasks);

        Ok(Some(PreparedServiceRemoval {
            record,
            task,
            grace_period: self.restart_policy.wave_stop_timeout,
            control_runtime: self
                .control_runtime
                .as_ref()
                .map(|runtime| runtime.handle().clone()),
            cleanup: self.cleanup_parts(),
        }))
    }

    async fn force_remove_service_instance(
        &mut self,
        handle: &ServiceInstanceHandle,
    ) -> ServiceResult<bool> {
        let Some(record) = self.instance_registry.get(handle.instance_id()) else {
            return Ok(false);
        };
        if !record_matches_handle(&record, handle) {
            return Ok(false);
        }
        if self.is_lifecycle_operation_pending(handle.instance_id()) {
            return Ok(false);
        }

        record.cancellation_token().cancel();
        let task = {
            self.running_tasks
                .lock()
                .await
                .remove(&handle.instance_id())
        };
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }

        let terminated = ServiceStatus::Terminated;
        self.resources
            .status_plane
            .insert(handle.instance_id(), terminated.clone());
        self.resources
            .runtime_facts
            .record_service_status(handle.instance_id(), &terminated);
        cleanup_service_instance(self.cleanup_parts(), handle.instance_id()).await;
        self.resources.status_changed.notify_waiters();
        Ok(true)
    }

    fn cleanup_parts(&self) -> ServiceInstanceCleanupParts {
        ServiceInstanceCleanupParts {
            instance_registry: self.instance_registry.clone(),
            running_tasks: self.running_tasks.clone(),
            resources: self.resources.clone(),
            diagnostics: self.diagnostics.clone(),
            removing_instances: self.removing_instances.clone(),
            stopping_instances: self.stopping_instances.clone(),
        }
    }

    fn owns_service_entry(&self, entry_id: ServiceEntryId, entry: &'static ServiceEntry) -> bool {
        self.services
            .iter()
            .any(|service| service.entry_id == entry_id && std::ptr::eq(service.entry, entry))
    }

    fn is_lifecycle_operation_pending(&self, instance_id: ServiceInstanceId) -> bool {
        self.removing_instances.contains(&instance_id)
            || self.stopping_instances.contains(&instance_id)
    }
}

async fn finish_graceful_service_stop(prepared: PreparedServiceStop) -> ServiceResult<bool> {
    if let Some(mut task) = prepared.task {
        tokio::select! {
            result = &mut task => {
                if let Err(err) = result
                    && !err.is_cancelled()
                {
                    tracing::warn!(
                        service = %prepared.record.name(),
                        service_instance_id = %prepared.record.instance_id(),
                        error = ?err,
                        "Service instance ended unexpectedly while stopping"
                    );
                }
            }
            _ = tokio::time::sleep(prepared.grace_period) => {
                tracing::warn!(
                    service = %prepared.record.name(),
                    service_instance_id = %prepared.record.instance_id(),
                    "Service instance did not stop within grace period, forcing abort"
                );
                task.abort();
                let _ = task.await;
            }
        }
    }

    let terminated = ServiceStatus::Terminated;
    prepared
        .resources
        .status_plane
        .insert(prepared.record.instance_id(), terminated.clone());
    prepared
        .resources
        .runtime_facts
        .record_service_status(prepared.record.instance_id(), &terminated);
    prepared
        .stopping_instances
        .remove(&prepared.record.instance_id());
    prepared.resources.status_changed.notify_waiters();
    Ok(true)
}

async fn finish_graceful_service_removal(prepared: PreparedServiceRemoval) -> ServiceResult<bool> {
    if let Some(mut task) = prepared.task {
        tokio::select! {
            result = &mut task => {
                if let Err(err) = result
                    && !err.is_cancelled()
                {
                    tracing::warn!(
                        service = %prepared.record.name(),
                        service_instance_id = %prepared.record.instance_id(),
                        error = ?err,
                        "Service instance ended unexpectedly while removing"
                    );
                }
            }
            _ = tokio::time::sleep(prepared.grace_period) => {
                tracing::warn!(
                    service = %prepared.record.name(),
                    service_instance_id = %prepared.record.instance_id(),
                    "Service instance did not stop within grace period, forcing abort"
                );
                task.abort();
                let _ = task.await;
            }
        }
    }

    cleanup_service_instance(prepared.cleanup, prepared.record.instance_id()).await;
    Ok(true)
}

async fn cleanup_service_instance(
    cleanup: ServiceInstanceCleanupParts,
    instance_id: ServiceInstanceId,
) {
    cleanup.instance_registry.remove(instance_id);
    cleanup.running_tasks.lock().await.remove(&instance_id);
    cleanup.resources.status_plane.remove(&instance_id);
    cleanup.resources.shelf.remove(&instance_id);
    cleanup.resources.reload_signals.remove(&instance_id);
    cleanup
        .resources
        .runtime_facts
        .remove_service_instance(instance_id);
    cleanup.diagnostics.remove_service_instance(instance_id);
    cleanup.removing_instances.remove(&instance_id);
    cleanup.stopping_instances.remove(&instance_id);
    cleanup.resources.status_changed.notify_waiters();
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

fn record_matches_handle(record: &ServiceInstanceRecord, handle: &ServiceInstanceHandle) -> bool {
    record.instance_id() == handle.instance_id()
        && record.entry_id() == handle.entry_id()
        && std::ptr::eq(record.entry(), handle.entry())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        HighPriorityRuntimePolicy, HighPriorityShardId, ProviderEntry, ProviderInitError, Registry,
        ServiceEntry, ServiceEntryId, ServiceInstanceHandle, ServiceInstanceRecord,
        ServiceInstanceRegistry, ServiceParam, ServiceScheduling,
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
    use std::time::{Duration, Instant};
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
        _: crate::models::ServiceInvocationContext,
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
        input: None,
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
        input: None,
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
        input: None,
        tags: &["__unit_runtime_isolated__"],
    };

    fn test_service(id: usize, entry: &'static ServiceEntry) -> ServiceDescription {
        let entry_id = ServiceEntryId::new(id);
        let instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(id as u128));
        let instance_registry = Arc::new(ServiceInstanceRegistry::new());
        instance_registry.insert(ServiceInstanceRecord::new(
            instance_id,
            entry_id,
            entry,
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
        let daemon = ServiceDaemon::builder().with_registry(registry).build();
        let service_handle = daemon
            .service_instances()
            .into_iter()
            .find(|instance| instance.name() == "capacity_primary_standard_service")
            .expect("standard test service should have one auto-start instance")
            .service();
        let expected_instances = daemon.service_instances_for(&service_handle);
        let expected_instance = expected_instances
            .first()
            .cloned()
            .expect("selected service should have one auto-start instance");

        assert!(daemon.service_instances().contains(&expected_instance));
        assert_eq!(expected_instances, vec![expected_instance]);

        daemon_registry().unregister(daemon.id());
    }

    #[test]
    fn service_handle_is_scoped_to_owning_daemon() {
        let registry_a = Registry::builder()
            .with_tag("__unit_high_priority_capacity_primary__")
            .build();
        let registry_b = Registry::builder()
            .with_tag("__unit_high_priority_capacity_primary__")
            .build();
        let daemon_a = ServiceDaemon::builder().with_registry(registry_a).build();
        let daemon_b = ServiceDaemon::builder().with_registry(registry_b).build();
        let service_handle = daemon_a
            .service_instances()
            .into_iter()
            .find(|instance| instance.name() == "capacity_primary_standard_service")
            .expect("standard test service should have one auto-start instance")
            .service();

        assert_eq!(service_handle.daemon_id(), daemon_a.id());
        assert_eq!(service_handle.instances().len(), 1);
        assert!(daemon_b.service_instances_for(&service_handle).is_empty());

        daemon_registry().unregister(daemon_a.id());
        daemon_registry().unregister(daemon_b.id());
    }

    #[test]
    fn service_handle_does_not_keep_daemon_control_alive() {
        let service_handle = {
            let registry = Registry::builder()
                .with_tag("__unit_high_priority_capacity_primary__")
                .build();
            let daemon = ServiceDaemon::builder().with_registry(registry).build();
            let handle = daemon
                .service_instances()
                .into_iter()
                .find(|instance| instance.name() == "capacity_primary_standard_service")
                .expect("standard test service should have one auto-start instance")
                .service();

            daemon_registry().unregister(daemon.id());
            handle
        };

        assert!(service_handle.instances().is_empty());
    }

    #[tokio::test]
    async fn service_handle_create_requires_running_daemon() {
        let registry = Registry::builder()
            .with_tag("__unit_high_priority_capacity_primary__")
            .build();
        let daemon = ServiceDaemon::builder().with_registry(registry).build();
        let service_handle = daemon
            .service_instances()
            .into_iter()
            .find(|instance| instance.name() == "capacity_primary_standard_service")
            .expect("standard test service should have one auto-start instance")
            .service();

        let err = service_handle
            .create(())
            .await
            .expect_err("create should require daemon run() to have started");
        assert!(
            err.to_string()
                .contains("cannot create service instance before daemon run() starts"),
            "unexpected create error: {err}"
        );

        daemon_registry().unregister(daemon.id());
    }

    #[tokio::test]
    async fn service_instance_handle_uses_registry_owned_control_until_unregister() {
        let registry = Registry::builder()
            .with_tag("__unit_high_priority_capacity_primary__")
            .build();
        let daemon = ServiceDaemon::builder().with_registry(registry).build();
        let daemon_id = daemon.id();
        let instance = daemon
            .service_instances()
            .into_iter()
            .find(|instance| instance.name() == "capacity_primary_standard_service")
            .expect("standard test service should have one auto-start instance");
        let service = instance.service();

        drop(daemon);

        assert_eq!(instance.daemon_id(), daemon_id);
        assert_eq!(instance.status().await, ServiceStatus::Initializing);
        assert_eq!(service.instances().len(), 1);

        daemon_registry().unregister(daemon_id);

        assert_eq!(instance.status().await, ServiceStatus::Terminated);
        assert!(instance.runtime().is_none());
        assert!(instance.trigger_runtime().is_none());
        assert!(!instance.request_stop());
        assert!(service.instances().is_empty());
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

        assert!(daemon.high_priority_runtime_pool.is_empty());
    }

    #[test]
    fn ensure_high_priority_runtime_skips_standard_only_services() {
        let mut daemon = test_inner_builder()
            .with_registry(isolated_registry())
            .build_inner();
        daemon.services = vec![test_service(1, &STANDARD_TEST_ENTRY)];
        daemon.high_priority_capacity = HighPriorityCapacityPlan::from_services(&daemon.services);
        daemon.high_priority_runtime_pool = HighPriorityRuntimePool::new(
            daemon.high_priority_runtime_pool.policy(),
            daemon.high_priority_capacity,
        );

        let runtime = daemon
            .ensure_high_priority_runtime()
            .expect("runtime check should not fail for standard-only services");

        assert!(runtime.is_none());
        assert!(daemon.high_priority_runtime_pool.is_empty());
    }

    #[test]
    fn ensure_high_priority_runtime_creates_for_high_priority_services() {
        let mut daemon = test_inner_builder()
            .with_registry(isolated_registry())
            .build_inner();
        daemon.services = vec![test_service(1, &HIGH_PRIORITY_TEST_ENTRY)];
        daemon.high_priority_capacity = HighPriorityCapacityPlan::from_services(&daemon.services);
        daemon.high_priority_runtime_pool = HighPriorityRuntimePool::new(
            daemon.high_priority_runtime_pool.policy(),
            daemon.high_priority_capacity,
        );

        let runtime = daemon
            .ensure_high_priority_runtime()
            .expect("runtime creation should succeed for high-priority services");

        assert!(runtime.is_some());
        assert!(!daemon.high_priority_runtime_pool.is_empty());
        daemon.shutdown_high_priority_runtime();
        assert!(daemon.high_priority_runtime_pool.is_empty());
    }

    #[tokio::test]
    async fn run_keeps_high_priority_runtime_absent_for_empty_registry() {
        let mut daemon = test_inner_builder()
            .with_registry(isolated_registry())
            .build_inner();

        daemon.run().await;

        assert!(daemon.high_priority_runtime_pool.is_empty());
    }

    #[test]
    fn shutdown_high_priority_runtime_is_noop_when_never_created() {
        let mut daemon = test_inner_builder()
            .with_registry(isolated_registry())
            .build_inner();

        daemon.shutdown_high_priority_runtime();

        assert!(daemon.high_priority_runtime_pool.is_empty());
    }

    #[test]
    fn high_priority_policy_tick_scales_out_after_sustained_shard_pressure() {
        let mut daemon = test_inner_builder()
            .with_registry(
                Registry::builder()
                    .with_tag("__unit_high_priority_capacity_primary__")
                    .build(),
            )
            .with_high_priority_runtime_policy(HighPriorityRuntimePolicy::for_testing())
            .build_inner();
        daemon
            .ensure_high_priority_runtime()
            .expect("runtime should build");
        daemon.diagnostics.record_high_priority_shard_observation(
            HighPriorityShardId(0),
            crate::core::diagnostics::SleepObservation {
                source: crate::core::diagnostics::SleepObservationSource::RuntimeProbe,
                reason: crate::core::diagnostics::SleepExitReason::Completed,
                requested: Duration::from_millis(250),
                elapsed: Duration::from_millis(270),
                drift: Duration::from_millis(20),
            },
        );

        daemon.evaluate_high_priority_runtime_policy(Instant::now());

        let runtime = daemon.resources.runtime_facts.daemon_snapshot(false);
        assert_eq!(runtime.high_priority_shards.len(), 2);
        assert!(runtime.high_priority_shards.iter().any(|shard| {
            shard.shard_id == HighPriorityShardId(0)
                && shard.pressure_state == crate::models::HighPriorityShardPressureState::Pressured
        }));
        let diagnostics: crate::models::DaemonDiagnosticsSnapshot =
            daemon.diagnostics.snapshot().into();
        assert!(
            diagnostics
                .high_priority_placement_decisions
                .iter()
                .any(|decision| decision.kind
                    == crate::models::DiagnosticHighPriorityPlacementDecisionKind::ScaleOut)
        );

        daemon.shutdown_high_priority_runtime();
    }

    #[tokio::test]
    async fn do_shutdown_drops_high_priority_runtime_before_control_runtime() {
        let drop_order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let control_drop_order = drop_order.clone();
        let mut daemon = test_inner_builder()
            .with_registry(isolated_registry())
            .build_inner();

        daemon.services = vec![test_service(1, &HIGH_PRIORITY_TEST_ENTRY)];
        daemon.high_priority_capacity = HighPriorityCapacityPlan::from_services(&daemon.services);
        daemon.high_priority_runtime_pool = HighPriorityRuntimePool::new(
            daemon.high_priority_runtime_pool.policy(),
            daemon.high_priority_capacity,
        );
        daemon
            .ensure_high_priority_runtime()
            .expect("high-priority runtime should build");
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

        assert!(daemon.high_priority_runtime_pool.is_empty());
        assert!(daemon.control_runtime.is_none());
        assert_eq!(
            drop_order
                .lock()
                .expect("drop order mutex should not be poisoned")
                .as_slice(),
            ["control"]
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
            handle.control.clone(),
        );

        let status = unknown_instance.status().await;
        assert_eq!(status, ServiceStatus::Terminated);
    }

    #[tokio::test]
    async fn daemon_handle_reads_status_by_instance_handle() {
        setup_tracing();
        let registry = Registry::builder()
            .with_tag("__unit_high_priority_capacity_primary__")
            .build();
        let handle = ServiceDaemon::builder().with_registry(registry).build();
        let service_handle = handle
            .service_instances()
            .into_iter()
            .find(|instance| instance.name() == "capacity_primary_standard_service")
            .expect("standard test service should have one instance")
            .service();
        let instance = handle
            .service_instances_for(&service_handle)
            .into_iter()
            .next()
            .expect("standard test service should have one instance");

        let status = instance.status().await;
        assert_eq!(status, ServiceStatus::Initializing);

        handle
            .control
            .resources
            .status_plane
            .insert(instance.instance_id(), ServiceStatus::Healthy);
        let status = instance.status().await;
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
            standard_instance.runtime().map(|snapshot| snapshot.status),
            Some(ServiceStatus::Healthy)
        );
        let fake_instance = ServiceInstanceHandle::new(
            ServiceInstanceId::new(uuid::Uuid::from_u128(999)),
            standard_instance.entry_id(),
            standard_instance.entry(),
            handle.control.clone(),
        );
        assert!(fake_instance.runtime().is_none());

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
        let handle = ServiceDaemon::builder().with_registry(registry).build();
        let service_handle = handle
            .service_instances()
            .into_iter()
            .find(|instance| instance.name() == "capacity_primary_standard_service")
            .expect("standard test service should have one instance")
            .service();
        let instance = handle
            .service_instances_for(&service_handle)
            .into_iter()
            .next()
            .expect("auto-start service should have an instance handle");
        let fake_instance = ServiceInstanceHandle::new(
            ServiceInstanceId::new(uuid::Uuid::from_u128(999)),
            instance.entry_id(),
            instance.entry(),
            handle.control.clone(),
        );

        assert!(!fake_instance.request_stop());
        assert_eq!(fake_instance.status().await, ServiceStatus::Terminated);

        assert!(instance.request_stop());
        assert_eq!(instance.status().await, ServiceStatus::ShuttingDown);
        assert_eq!(
            instance.runtime().map(|snapshot| snapshot.status),
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
