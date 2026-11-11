use dashmap::DashMap;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::core::context::{DaemonResources, process_token};
use crate::core::diagnostics::DiagnosticsStore;
#[cfg(feature = "high-priority")]
use crate::models::SchedulingAdvisoryProfile;
#[cfg(feature = "high-priority")]
use crate::models::policy::HighPriorityRuntimeControl;
use crate::models::{DaemonInstanceId, Registry, ServiceDescription};

#[cfg(feature = "high-priority")]
use super::high_priority::HighPriorityRuntimePool;
use super::policy::RestartPolicy;
#[cfg(feature = "high-priority")]
use super::runtime::HighPriorityCapacityPlan;
use super::runtime::ISOLATED_STARTUP_CONCURRENCY_LIMIT;
use super::{DaemonInstanceHandle, DaemonInstanceInner, daemon_registry};

/// Builder for constructing a daemon instance.
///
/// The `.build()` method is **infallible** -- it always returns a valid daemon.
pub struct ServiceDaemonBuilder {
    registry: Option<Registry>,
    restart_policy: RestartPolicy,
    /// External cancellation token for hierarchical lifecycle management.
    external_cancel_token: Option<CancellationToken>,
    /// Type-erased trigger configuration overrides.
    trigger_configs: DashMap<TypeId, Box<dyn Any + Send + Sync>>,
    #[cfg(feature = "high-priority")]
    scheduling_advisory_profile: SchedulingAdvisoryProfile,
    #[cfg(feature = "high-priority")]
    high_priority_runtime_control: HighPriorityRuntimeControl,
    isolated_startup_concurrency_limit: usize,
    /// Infrastructure tags whose services are always included in the final
    /// registry, regardless of the user-provided tag filters. Used by
    /// `MockContext` to auto-include `log_service` in simulation tests.
    infra_tags: Vec<&'static str>,
    /// Pre-filled resources for simulation (only available with `simulation` feature).
    #[cfg(feature = "simulation")]
    resources: Option<Arc<DaemonResources>>,
}

impl ServiceDaemonBuilder {
    pub(super) fn new() -> Self {
        Self {
            registry: None,
            restart_policy: RestartPolicy::default(),
            external_cancel_token: None,
            trigger_configs: DashMap::new(),
            #[cfg(feature = "high-priority")]
            scheduling_advisory_profile: SchedulingAdvisoryProfile::default(),
            #[cfg(feature = "high-priority")]
            high_priority_runtime_control: HighPriorityRuntimeControl::default(),
            isolated_startup_concurrency_limit: ISOLATED_STARTUP_CONCURRENCY_LIMIT,
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
            #[cfg(feature = "high-priority")]
            scheduling_advisory_profile: SchedulingAdvisoryProfile::default(),
            #[cfg(feature = "high-priority")]
            high_priority_runtime_control: HighPriorityRuntimeControl::default(),
            isolated_startup_concurrency_limit: ISOLATED_STARTUP_CONCURRENCY_LIMIT,
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

    /// Set the scheduling advisory profile.
    ///
    /// This controls advisory diagnostics emission only. It does not change
    /// service lifecycle, declared scheduling modes, or body placement.
    #[must_use]
    #[cfg(feature = "high-priority")]
    pub fn with_scheduling_advisory_profile(mut self, profile: SchedulingAdvisoryProfile) -> Self {
        self.scheduling_advisory_profile = profile;
        self
    }

    #[cfg(all(test, feature = "high-priority"))]
    pub(crate) fn with_test_high_priority_runtime_control(
        mut self,
        control: HighPriorityRuntimeControl,
    ) -> Self {
        self.high_priority_runtime_control = control;
        self
    }

    /// Set the maximum number of isolated generations admitted to startup at once.
    ///
    /// This covers isolated startup allocation only: permit acquisition, OS
    /// thread spawn, and private Tokio runtime creation. It does not limit how
    /// many isolated generation bodies may keep running after startup.
    #[must_use]
    pub fn with_isolated_startup_concurrency_limit(mut self, limit: NonZeroUsize) -> Self {
        self.isolated_startup_concurrency_limit = limit.get();
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
    pub(crate) fn with_resources(mut self, resources: Arc<DaemonResources>) -> Self {
        self.resources = Some(resources);
        self
    }

    /// Link the daemon to an external `CancellationToken` for hierarchical
    /// lifecycle management.
    ///
    /// When the external token is cancelled, the daemon will treat it as a
    /// shutdown signal and begin graceful termination. Conversely, when the
    /// daemon handle's [`shutdown()`](DaemonInstanceHandle::shutdown) is called, it will
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
    /// let daemon = ServiceDaemon::builder()
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
    /// `ServiceEntryId`.
    #[must_use]
    pub fn with_infra_tags(mut self, tags: &[&'static str]) -> Self {
        self.infra_tags.extend_from_slice(tags);
        self
    }

    /// Build and register a daemon instance.
    ///
    /// This method is **infallible** -- it always returns a valid daemon.
    /// If no registry was provided, all statically registered services are included.
    ///
    /// Provider dependency cycles are checked later in [`DaemonInstanceHandle::run`]
    /// (not here) so that `build()` stays allocation-only and non-blocking.
    /// A cycle surfaces as a `tracing::error!` followed by `shutdown()`; users
    /// observe the outcome via the daemon handle / status plane.
    #[must_use]
    pub fn build(self) -> DaemonInstanceHandle {
        let inner = self.build_inner();
        daemon_registry().register(inner)
    }

    pub(crate) fn build_inner(self) -> DaemonInstanceInner {
        let registry = self.registry.unwrap_or_else(|| Registry::builder().build());
        let (mut services, mut projection, instance_registry) = registry.into_parts();

        // Merge infrastructure services that bypass tag filtering.
        // Each infra tag is resolved against the global SERVICE_REGISTRY,
        // and matching services are appended (deduplicated by ServiceEntryId).
        if !self.infra_tags.is_empty() {
            let infra_registry = Registry::builder().with_tags(self.infra_tags).build();
            let (infra_services, infra_projection, _) = infra_registry.into_parts();
            for service in infra_services {
                if !services
                    .iter()
                    .any(|existing| existing.entry_id == service.entry_id)
                {
                    for record in service.instance_records() {
                        instance_registry.insert(record);
                    }
                    services.push(ServiceDescription {
                        entry_id: service.entry_id,
                        entry: service.entry,
                        instance_registry: instance_registry.clone(),
                    });
                }
            }
            projection = projection.merge(&infra_projection);
            services.sort_by_key(|service| service.entry_id);
        }

        #[cfg(feature = "high-priority")]
        let high_priority_capacity = HighPriorityCapacityPlan::from_services(&services);
        #[cfg(feature = "high-priority")]
        let high_priority_runtime_pool = HighPriorityRuntimePool::new(
            self.high_priority_runtime_control,
            #[cfg(feature = "high-priority")]
            high_priority_capacity,
        );

        let daemon_id = DaemonInstanceId::new_v7();
        #[cfg(feature = "simulation")]
        let resources = self.resources.unwrap_or_else(|| {
            DaemonResources::new_with_diagnostics_for_daemon(
                Arc::new(DiagnosticsStore::new()),
                daemon_id,
            )
        });
        #[cfg(not(feature = "simulation"))]
        let resources = DaemonResources::new_with_diagnostics_for_daemon(
            Arc::new(DiagnosticsStore::new()),
            daemon_id,
        );
        resources.set_service_catalog_projection(projection);
        let diagnostics = resources.diagnostics.clone();
        resources
            .runtime_facts
            .register_service_instances(&instance_registry.records());

        // Inject daemon-level trigger configs into the shared resources.
        resources
            .trigger_configs
            .insert(TypeId::of::<RestartPolicy>(), Box::new(self.restart_policy));
        for entry in self.trigger_configs {
            resources.trigger_configs.insert(entry.0, entry.1);
        }

        DaemonInstanceInner {
            services,
            instance_registry,
            removing_instances: Arc::new(dashmap::DashSet::new()),
            stopping_instances: Arc::new(dashmap::DashSet::new()),
            running_tasks: Arc::new(Mutex::new(HashMap::new())),
            restart_policy: self.restart_policy,
            cancellation_token: process_token().child_token(),
            control_runtime: None,
            standard_runtime: None,
            #[cfg(feature = "high-priority")]
            high_priority_capacity,
            #[cfg(feature = "high-priority")]
            high_priority_runtime_pool,
            #[cfg(feature = "high-priority")]
            runtime_probe_tasks: Vec::new(),
            #[cfg(feature = "high-priority")]
            adaptive_recommendation_task: None,
            #[cfg(feature = "high-priority")]
            high_priority_policy_task: None,
            #[cfg(feature = "high-priority")]
            scheduling_advisory_profile: self.scheduling_advisory_profile,
            external_cancel_token: self.external_cancel_token,
            resources,
            diagnostics,
            isolated_startup_permits: Arc::new(Semaphore::new(
                self.isolated_startup_concurrency_limit,
            )),
            startup_gate: Arc::default(),
        }
    }
}
