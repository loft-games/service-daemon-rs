//! ServiceDaemon - the main orchestrator for managed services.
//!
//! This module is split into submodules for better organization:
//! - `policy`: Restart policy configuration.
//! - `runner`: Service spawning and lifecycle management.

mod parts;
mod policy;
mod runner;

use dashmap::DashMap;
use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::pending;
use std::sync::Arc;
#[cfg(feature = "simulation")]
use std::time::Duration;
#[cfg(all(feature = "simulation", test))]
use std::time::Instant;
use tokio::runtime::{Handle, Runtime};
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument};

use petgraph::{
    algo::toposort,
    graph::{DiGraph, NodeIndex},
};

#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};

use crate::core::context::{DaemonResources, process_token};
use crate::core::diagnostics::{DiagnosticsStore, RuntimeLane, run_lane_runtime_probe};
#[cfg(any(unix, feature = "simulation"))]
use crate::models::ServiceError;
use crate::models::{
    PROVIDER_REGISTRY, ProviderEntry, ProviderInitError, Registry, Result as ServiceResult,
    ServiceDescription, ServiceId, ServiceScheduling, ServiceStatus,
};

pub use policy::{RestartPolicy, RestartPolicyBuilder};

const ISOLATED_STARTUP_CONCURRENCY_LIMIT: usize = 4;

// ---------------------------------------------------------------------------
// ServiceDaemonHandle -- lightweight status query interface
// ---------------------------------------------------------------------------

/// A handle to the ServiceDaemon that can be used to query status and interact with services.
#[derive(Clone)]
pub struct ServiceDaemonHandle {
    resources: Arc<DaemonResources>,
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
    /// Shared runtime lazily created for HighPriority services.
    high_priority_runtime: Option<Runtime>,
    runtime_probe_tasks: Vec<JoinHandle<()>>,
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
        self.shutdown_high_priority_runtime_detached();
    }
}

impl ServiceDaemon {
    /// Start building a new `ServiceDaemon`.
    #[must_use]
    pub fn builder() -> ServiceDaemonBuilder {
        ServiceDaemonBuilder::new()
    }

    async fn eager_init_reachable_providers(&self) -> Result<(), ProviderInitError> {
        let mut providers_by_id: HashMap<TypeId, &'static ProviderEntry> = HashMap::new();
        for entry in PROVIDER_REGISTRY.iter() {
            providers_by_id.insert(entry.type_id, entry);
        }

        // 1) Collect initial reachable set from service parameters.
        let mut reachable: HashSet<TypeId> = HashSet::new();
        let mut queue: VecDeque<TypeId> = VecDeque::new();
        for svc in &self.services {
            for p in svc.params() {
                if reachable.insert(p.type_id) {
                    queue.push_back(p.type_id);
                }
            }
        }

        // 2) Expand via provider->provider edges.
        while let Some(tid) = queue.pop_front() {
            let Some(p) = providers_by_id.get(&tid) else {
                continue;
            };
            for dep in p.params {
                if reachable.insert(dep.type_id) {
                    queue.push_back(dep.type_id);
                }
            }
        }

        // 3) Filter eager targets.
        let eager_targets: Vec<&'static ProviderEntry> = reachable
            .iter()
            .filter_map(|tid| providers_by_id.get(tid).copied())
            .filter(|p| p.eager)
            .collect();

        if eager_targets.is_empty() {
            return Ok(());
        }

        // 4) Toposort reachable provider DAG to get a deterministic init order.
        // Nodes are provider TypeIds; edges are dep -> provider.
        let mut graph = DiGraph::<TypeId, ()>::new();
        let mut nodes: HashMap<TypeId, NodeIndex> = HashMap::new();

        for tid in reachable.iter().copied() {
            if providers_by_id.contains_key(&tid) {
                nodes.entry(tid).or_insert_with(|| graph.add_node(tid));
            }
        }

        for (&tid, entry) in providers_by_id.iter() {
            if !reachable.contains(&tid) {
                continue;
            }
            let Some(&prov_node) = nodes.get(&tid) else {
                continue;
            };
            for dep in entry.params {
                if !reachable.contains(&dep.type_id) {
                    continue;
                }
                if let Some(&dep_node) = nodes.get(&dep.type_id) {
                    graph.add_edge(dep_node, prov_node, ());
                }
            }
        }

        // Cycles are pre-checked by validate_dependency_graph() in run();
        // if we still land in Err here, report it as a Fatal init error
        // rather than panicking, as a defense-in-depth measure.
        let order = match toposort(&graph, None) {
            Ok(order) => order,
            Err(err) => {
                let offending = providers_by_id
                    .iter()
                    .find_map(|(tid, p)| (*tid == graph[err.node_id()]).then_some(p.name))
                    .unwrap_or("<unknown>");
                return Err(ProviderInitError::Fatal {
                    provider: offending.to_owned(),
                    message: "Circular provider dependency reached eager_init; \
                              this should have been caught by validate_dependency_graph"
                        .to_owned(),
                });
            }
        };

        // 5) Execute init in order, only for eager providers.
        let eager_ids: HashSet<TypeId> = eager_targets.iter().map(|p| p.type_id).collect();
        for node in order {
            let tid = graph[node];
            if !eager_ids.contains(&tid) {
                continue;
            }
            let entry = providers_by_id
                .get(&tid)
                .copied()
                .expect("provider must exist in providers_by_id");
            (entry.init)(self.restart_policy, self.cancellation_token.clone()).await?;
        }

        Ok(())
    }

    fn has_high_priority_services(&self) -> bool {
        self.services
            .iter()
            .any(|service| matches!(service.entry.scheduling, ServiceScheduling::HighPriority))
    }

    fn ensure_high_priority_runtime(&mut self) -> std::io::Result<Option<Handle>> {
        if !self.has_high_priority_services() {
            return Ok(None);
        }

        if self.high_priority_runtime.is_none() {
            self.high_priority_runtime = Some(
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .thread_name("svc-high-priority")
                    .build()?,
            );
        }

        Ok(self
            .high_priority_runtime
            .as_ref()
            .map(|runtime| runtime.handle().clone()))
    }

    fn spawn_runtime_probe(&mut self, handle: &Handle, lane: RuntimeLane) {
        let diagnostics = self.diagnostics.clone();
        let token = self.cancellation_token.clone();
        self.runtime_probe_tasks
            .push(handle.spawn(run_lane_runtime_probe(diagnostics, lane, token)));
    }

    async fn stop_runtime_probes(&mut self) {
        for handle in self.runtime_probe_tasks.drain(..) {
            if let Err(err) = handle.await
                && !err.is_cancelled()
            {
                tracing::warn!(error = ?err, "Runtime probe task ended unexpectedly");
            }
        }
    }

    /// Get the cancellation token for this daemon.
    pub fn cancel_token(&self) -> tokio_util::sync::CancellationToken {
        self.cancellation_token.clone()
    }

    /// Get a handle to the daemon for querying status.
    pub fn handle(&self) -> ServiceDaemonHandle {
        ServiceDaemonHandle {
            resources: self.resources.clone(),
        }
    }

    /// **[Simulation Only]** Returns a clone of the daemon's internal resources.
    ///
    /// This is used by `SimulationHandle` to perform dynamic injection ("SimulationHandle")
    /// during a running simulation. Since `DaemonResources` uses `Arc` internally,
    /// modifications through the returned clone are immediately visible to all services.
    ///
    /// # Safety
    /// This method is gated behind the `simulation` feature to prevent misuse
    /// in production environments.
    #[cfg(feature = "simulation")]
    pub fn resources(&self) -> Arc<DaemonResources> {
        self.resources.clone()
    }

    /// Get the current status of a service by its `ServiceId`.
    pub async fn get_service_status(&self, id: &ServiceId) -> ServiceStatus {
        self.handle().get_service_status(id).await
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

        // Validate the provider dependency graph. Cycles in the provider graph
        // would deadlock at runtime, so we surface them as a pre-startup failure
        // that triggers a graceful shutdown (observable via the daemon handle /
        // status plane rather than blocking `run()`).
        if let Err(err) = validate_dependency_graph(&self.services, PROVIDER_REGISTRY.iter()) {
            tracing::error!(error = %err, "ServiceDaemon provider dependency graph validation failed");
            self.shutdown();
            return self;
        }

        // Eager-initialize reachable providers before spawning services.
        //
        // This is opt-in (providers must specify `eager = true`).
        if let Err(err) = self.eager_init_reachable_providers().await {
            tracing::error!(error = %err, "ServiceDaemon eager provider initialization failed");
            self.shutdown();
            return self;
        }

        let high_priority_runtime = match self.ensure_high_priority_runtime() {
            Ok(runtime) => runtime,
            Err(err) => {
                tracing::error!(error = %err, "ServiceDaemon high-priority runtime creation failed");
                self.shutdown();
                return self;
            }
        };

        self.spawn_runtime_probe(&Handle::current(), RuntimeLane::Standard);
        if let Some(runtime) = high_priority_runtime.as_ref() {
            self.spawn_runtime_probe(runtime, RuntimeLane::HighPriority);
        }

        // Spawn all services in the background
        runner::spawn_all_services(parts::SpawnAllServicesParts {
            services: &self.services,
            restart_policy: self.restart_policy,
            running_tasks: self.running_tasks.clone(),
            resources: self.resources.clone(),
            diagnostics: self.diagnostics.clone(),
            isolated_startup_permits: self.isolated_startup_permits.clone(),
            high_priority_runtime,
            daemon_token: &self.cancellation_token,
        })
        .await;

        #[cfg(feature = "diagnostics")]
        super::topology_collector::start_topology_collector();

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
        runner::stop_all_services(
            &self.services,
            self.running_tasks.clone(),
            self.resources.clone(),
            self.cancellation_token.clone(),
            self.restart_policy.wave_stop_timeout,
        )
        .await;

        self.stop_runtime_probes().await;
        self.shutdown_high_priority_runtime();

        #[cfg(feature = "diagnostics")]
        if let Some(mermaid) = super::topology_collector::export_mermaid() {
            println!("\n=== BEHAVIORAL TOPOLOGY (MERMAID) ===\n");
            println!("{}\n", mermaid);
            println!("====================================\n");
        }

        info!("ServiceDaemon stopped.");
    }

    fn shutdown_high_priority_runtime(&mut self) {
        if let Some(runtime) = self.high_priority_runtime.take()
            && let Err(panic) = std::thread::spawn(move || drop(runtime)).join()
        {
            tracing::error!(?panic, "High-priority runtime shutdown thread panicked");
        }
    }

    fn shutdown_high_priority_runtime_detached(&mut self) {
        if let Some(runtime) = self.high_priority_runtime.take() {
            let _ = std::thread::spawn(move || drop(runtime));
        }
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
        let daemon_token = self.cancellation_token.clone();

        let high_priority_runtime = self
            .ensure_high_priority_runtime()
            .map_err(|err| ServiceError::InternalError(err.to_string()))?;

        self.spawn_runtime_probe(&Handle::current(), RuntimeLane::Standard);
        if let Some(runtime) = high_priority_runtime.as_ref() {
            self.spawn_runtime_probe(runtime, RuntimeLane::HighPriority);
        }

        for service in &self.services {
            let (supervisor_lane, generation_lane) = match service.entry.scheduling {
                ServiceScheduling::Standard => (
                    parts::SupervisorSpawnLane::Standard,
                    parts::GenerationExecutionLane::CurrentRuntime,
                ),
                ServiceScheduling::HighPriority => {
                    let Some(runtime) = high_priority_runtime.clone() else {
                        return Err(ServiceError::InternalError(format!(
                            "HighPriority service '{}' is missing the shared high-priority runtime",
                            service.name()
                        )));
                    };
                    (
                        parts::SupervisorSpawnLane::HighPriority(runtime),
                        parts::GenerationExecutionLane::CurrentRuntime,
                    )
                }
                ServiceScheduling::Isolated => (
                    parts::SupervisorSpawnLane::Standard,
                    parts::GenerationExecutionLane::Isolated,
                ),
            };

            runner::spawn_service(parts::SpawnServiceParts {
                service_id: service.id,
                name: service.name(),
                run: service.entry.wrapper,
                watcher: service.entry.watcher,
                policy: test_policy,
                scheduling: service.entry.scheduling,
                supervisor_lane,
                generation_lane,
                running_tasks: self.running_tasks.clone(),
                resources: self.resources.clone(),
                diagnostics: self.diagnostics.clone(),
                isolated_startup_permits: self.isolated_startup_permits.clone(),
                cancellation_token: service.cancellation_token.clone(),
                daemon_token: daemon_token.clone(),
            })
            .await;
        }

        tokio::time::sleep(duration).await;

        runner::stop_all_services(
            &self.services,
            self.running_tasks.clone(),
            self.resources.clone(),
            self.cancellation_token.clone(),
            test_policy.wave_stop_timeout,
        )
        .await;

        self.stop_runtime_probes().await;
        self.shutdown_high_priority_runtime_detached();

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ServiceDaemonBuilder -- Infallible, zero-config default
// ---------------------------------------------------------------------------

/// Builder for constructing a `ServiceDaemon`.
///
/// The `.build()` method is **infallible** -- it always returns a valid daemon.
pub struct ServiceDaemonBuilder {
    registry: Option<Registry>,
    restart_policy: RestartPolicy,
    /// External cancellation token for hierarchical lifecycle management.
    external_cancel_token: Option<CancellationToken>,
    /// Type-erased trigger configuration overrides.
    trigger_configs: DashMap<TypeId, Box<dyn Any + Send + Sync>>,
    /// Infrastructure tags whose services are always included in the final
    /// registry, regardless of the user-provided tag filters. Used by
    /// `MockContext` to auto-include `log_service` in simulation tests.
    infra_tags: Vec<&'static str>,
    /// Pre-filled resources for simulation (only available with `simulation` feature).
    #[cfg(feature = "simulation")]
    resources: Option<Arc<DaemonResources>>,
}

impl ServiceDaemonBuilder {
    fn new() -> Self {
        Self {
            registry: None,
            restart_policy: RestartPolicy::default(),
            external_cancel_token: None,
            trigger_configs: DashMap::new(),
            infra_tags: Vec::new(),
            #[cfg(feature = "simulation")]
            resources: None,
        }
    }

    /// **[Simulation Only]** Creates an isolated builder with an empty registry.
    ///
    /// This prevents auto-discovery of statically registered services, ensuring
    /// the simulation sandbox only runs explicitly added services.
    #[cfg(feature = "simulation")]
    pub(crate) fn new_isolated() -> Self {
        Self {
            registry: Some(
                Registry::builder()
                    .with_tag("__simulation_isolation__")
                    .build(),
            ),
            restart_policy: RestartPolicy::default(),
            external_cancel_token: None,
            trigger_configs: DashMap::new(),
            infra_tags: Vec::new(),
            resources: None,
        }
    }

    /// Use a pre-built `Registry` for service discovery.
    ///
    /// If not called, the daemon will automatically include all services
    /// discovered via the static `SERVICE_REGISTRY` (linkme).
    #[must_use]
    pub fn with_registry(mut self, registry: Registry) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Set a custom restart policy for the daemon.
    #[must_use]
    pub fn with_restart_policy(mut self, policy: RestartPolicy) -> Self {
        self.restart_policy = policy;
        self
    }

    /// **[Simulation Only]** Inject pre-filled `DaemonResources` into the daemon.
    ///
    /// This allows `MockContext` to pre-populate shelf data, status plane entries,
    /// and other resources before the daemon starts running services.
    ///
    /// # Safety
    /// This method is gated behind the `simulation` feature to prevent misuse
    /// in production environments.
    #[cfg(feature = "simulation")]
    #[must_use]
    pub fn with_resources(mut self, resources: Arc<DaemonResources>) -> Self {
        self.resources = Some(resources);
        self
    }

    /// Link the daemon to an external `CancellationToken` for hierarchical
    /// lifecycle management.
    ///
    /// When the external token is cancelled, the daemon will treat it as a
    /// shutdown signal and begin graceful termination. Conversely, when the
    /// daemon's [`shutdown()`](ServiceDaemon::shutdown) is called, it will
    /// also cancel this token, propagating the signal to all other components
    /// sharing it.
    #[must_use]
    pub fn with_cancel_token(mut self, token: CancellationToken) -> Self {
        self.external_cancel_token = Some(token);
        self
    }

    /// Register a trigger-specific configuration override.
    ///
    /// The registered config can be retrieved at runtime via
    /// [`context::trigger_config::<C>()`](crate::core::context::trigger_config).
    /// This is how users override the defaults declared by trigger templates
    /// (e.g. [`ScalingPolicy`](crate::models::ScalingPolicy)).
    ///
    /// This method can be called multiple times with different config types.
    /// Each call replaces the previous registration for that type.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let mut daemon = ServiceDaemon::builder()
    ///     .with_trigger_config(ScalingPolicy::builder()
    ///         .initial_concurrency(4)
    ///         .build())
    ///     .build();
    /// ```
    #[must_use]
    pub fn with_trigger_config<C: 'static + Clone + Send + Sync>(self, config: C) -> Self {
        self.trigger_configs
            .insert(TypeId::of::<C>(), Box::new(config));
        self
    }

    /// Registers infrastructure tags whose services are always included,
    /// regardless of the user-provided Registry's include filters.
    ///
    /// This is used internally by `MockContext` to auto-include framework
    /// services (e.g., `log_service`) in simulation tests. The tagged services
    /// are merged into the final service list in `build()`, deduplicated by
    /// `ServiceId`.
    #[must_use]
    pub fn with_infra_tags(mut self, tags: &[&'static str]) -> Self {
        self.infra_tags.extend_from_slice(tags);
        self
    }

    /// Build the `ServiceDaemon`.
    ///
    /// This method is **infallible** -- it always returns a valid daemon.
    /// If no registry was provided, all statically registered services are included.
    ///
    /// Provider dependency cycles are checked later in [`ServiceDaemon::run`]
    /// (not here) so that `build()` stays allocation-only and non-blocking.
    /// A cycle surfaces as a `tracing::error!` followed by `shutdown()`; users
    /// observe the outcome via the daemon handle / status plane.
    #[must_use]
    pub fn build(self) -> ServiceDaemon {
        let registry = self.registry.unwrap_or_else(|| Registry::builder().build());
        let mut services = registry.into_services();

        // Merge infrastructure services that bypass tag filtering.
        // Each infra tag is resolved against the global SERVICE_REGISTRY,
        // and matching services are appended (deduplicated by ServiceId).
        if !self.infra_tags.is_empty() {
            let infra_services = Registry::builder()
                .with_tags(self.infra_tags)
                .build()
                .into_services();
            for svc in infra_services {
                if !services.iter().any(|s| s.id == svc.id) {
                    services.push(svc);
                }
            }
        }

        #[cfg(feature = "simulation")]
        let resources = self.resources.unwrap_or_else(DaemonResources::new);
        #[cfg(not(feature = "simulation"))]
        let resources = DaemonResources::new();

        // Inject user-registered trigger configs into the shared resources.
        for entry in self.trigger_configs {
            resources.trigger_configs.insert(entry.0, entry.1);
        }

        ServiceDaemon {
            services,
            running_tasks: Arc::new(Mutex::new(HashMap::new())),
            restart_policy: self.restart_policy,
            cancellation_token: process_token().child_token(),
            high_priority_runtime: None,
            runtime_probe_tasks: Vec::new(),
            external_cancel_token: self.external_cancel_token,
            resources,
            diagnostics: Arc::new(DiagnosticsStore::new()),
            isolated_startup_permits: Arc::new(Semaphore::new(ISOLATED_STARTUP_CONCURRENCY_LIMIT)),
        }
    }
}

/// Validates the provider dependency graph for cycles.
///
/// Services themselves do not depend on each other; only providers depend on
/// other providers. This function builds a directed graph where:
/// - Services are included only as the **roots** that anchor reachability
///   (service -> provider edges).
/// - Providers are nodes; edges go from a provider to each of its dependency
///   provider types.
///
/// `petgraph::algo::toposort` then reports any cycle as an error. On success,
/// the dependency summary is logged for observability.
///
/// The `providers` iterator is injected (rather than read from the global
/// `PROVIDER_REGISTRY`) so unit tests can exercise the cycle path without
/// polluting the static slice.
fn validate_dependency_graph<'a>(
    services: &[ServiceDescription],
    providers: impl IntoIterator<Item = &'a ProviderEntry>,
) -> Result<(), ProviderInitError> {
    let providers: Vec<&ProviderEntry> = providers.into_iter().collect();

    let mut graph = DiGraph::<&str, ()>::new();
    let mut service_nodes: HashMap<&str, NodeIndex> = HashMap::new();
    let mut type_nodes: HashMap<TypeId, NodeIndex> = HashMap::new();

    // Phase 1: Service -> Provider edges (roots).
    for service in services {
        let svc_node = *service_nodes
            .entry(service.name())
            .or_insert_with(|| graph.add_node(service.name()));

        for param in service.params() {
            let type_node = *type_nodes
                .entry(param.type_id)
                .or_insert_with(|| graph.add_node(param.type_name));
            graph.add_edge(svc_node, type_node, ());
        }
    }

    // Phase 2: Provider -> Provider edges (cycle-bearing subgraph).
    for provider in &providers {
        let prov_node = *type_nodes
            .entry(provider.type_id)
            .or_insert_with(|| graph.add_node(provider.name));

        for param in provider.params {
            let dep_node = *type_nodes
                .entry(param.type_id)
                .or_insert_with(|| graph.add_node(param.type_name));
            graph.add_edge(prov_node, dep_node, ());
        }
    }

    match toposort(&graph, None) {
        Ok(_order) => {
            for service in services {
                if !service.params().is_empty() {
                    let dep_names: Vec<&str> =
                        service.params().iter().map(|p| p.type_name).collect();
                    info!(
                        service = %service.name(),
                        dependencies = ?dep_names,
                        "Service dependency edge"
                    );
                }
            }
            for provider in &providers {
                if !provider.params.is_empty() {
                    let dep_names: Vec<&str> =
                        provider.params.iter().map(|p| p.type_name).collect();
                    info!(
                        provider = %provider.name,
                        dependencies = ?dep_names,
                        "Provider dependency edge"
                    );
                }
            }
            info!(
                total_services = services.len(),
                total_providers = providers.len(),
                total_graph_nodes = graph.node_count(),
                total_graph_edges = graph.edge_count(),
                "Provider dependency graph validated - no cycles detected"
            );
            Ok(())
        }
        Err(cycle_node) => {
            let cycle_label = graph[cycle_node.node_id()];
            let involved: Vec<&str> = graph
                .node_indices()
                .filter(|&n| {
                    graph.contains_edge(n, cycle_node.node_id())
                        || graph.contains_edge(cycle_node.node_id(), n)
                })
                .map(|n| graph[n])
                .collect();

            Err(ProviderInitError::Fatal {
                provider: cycle_label.to_owned(),
                message: format!(
                    "Circular dependency detected in provider dependency graph. \
                     Cycle involves '{cycle_label}', related nodes: {involved:?}. \
                     This would deadlock at runtime; review the #[provider] chain for these types."
                ),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ServiceEntry, ServiceParam};
    use crate::service;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;
    use tracing::debug;

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

    fn test_service(id: usize, entry: &'static ServiceEntry) -> ServiceDescription {
        ServiceDescription {
            id: ServiceId::new(id),
            entry,
            cancellation_token: CancellationToken::new(),
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
