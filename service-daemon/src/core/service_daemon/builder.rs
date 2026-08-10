use dashmap::DashMap;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::core::context::{DaemonResources, process_token};
use crate::core::diagnostics::DiagnosticsStore;
use crate::models::{Registry, SchedulingAdvisoryProfile};

use super::ServiceDaemon;
use super::policy::RestartPolicy;
use super::runtime::{HighPriorityCapacityPlan, ISOLATED_STARTUP_CONCURRENCY_LIMIT};

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
    scheduling_advisory_profile: SchedulingAdvisoryProfile,
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
            scheduling_advisory_profile: SchedulingAdvisoryProfile::default(),
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
            scheduling_advisory_profile: SchedulingAdvisoryProfile::default(),
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
    pub fn with_scheduling_advisory_profile(mut self, profile: SchedulingAdvisoryProfile) -> Self {
        self.scheduling_advisory_profile = profile;
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
    /// `ServiceEntryId`.
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
        let (mut services, mut projection, instance_registry) = registry.into_parts();

        // Merge infrastructure services that bypass tag filtering.
        // Each infra tag is resolved against the global SERVICE_REGISTRY,
        // and matching services are appended (deduplicated by ServiceEntryId).
        if !self.infra_tags.is_empty() {
            let infra_registry = Registry::builder().with_tags(self.infra_tags).build();
            let (infra_services, infra_projection, infra_instance_registry) =
                infra_registry.into_parts();
            for service in infra_services {
                if !services
                    .iter()
                    .any(|existing| existing.entry_id == service.entry_id)
                {
                    if let Some(record) = infra_instance_registry.get(service.instance_id) {
                        instance_registry.insert(record);
                    }
                    services.push(service);
                }
            }
            projection = projection.merge(&infra_projection);
        }

        let high_priority_capacity = HighPriorityCapacityPlan::from_services(&services);

        #[cfg(feature = "simulation")]
        let resources = self.resources.unwrap_or_else(|| {
            DaemonResources::new_with_diagnostics(Arc::new(DiagnosticsStore::new()))
        });
        #[cfg(not(feature = "simulation"))]
        let resources = DaemonResources::new_with_diagnostics(Arc::new(DiagnosticsStore::new()));
        resources.set_service_catalog_projection(projection);
        let diagnostics = resources.diagnostics.clone();
        resources.runtime_facts.register_services(&services);

        // Inject daemon-level trigger configs into the shared resources.
        resources
            .trigger_configs
            .insert(TypeId::of::<RestartPolicy>(), Box::new(self.restart_policy));
        for entry in self.trigger_configs {
            resources.trigger_configs.insert(entry.0, entry.1);
        }

        ServiceDaemon {
            services,
            instance_registry,
            running_tasks: Arc::new(Mutex::new(HashMap::new())),
            restart_policy: self.restart_policy,
            cancellation_token: process_token().child_token(),
            control_runtime: None,
            high_priority_capacity,
            high_priority_runtime: None,
            runtime_probe_tasks: Vec::new(),
            adaptive_recommendation_task: None,
            scheduling_advisory_profile: self.scheduling_advisory_profile,
            external_cancel_token: self.external_cancel_token,
            resources,
            diagnostics,
            isolated_startup_permits: Arc::new(Semaphore::new(
                self.isolated_startup_concurrency_limit,
            )),
        }
    }
}
