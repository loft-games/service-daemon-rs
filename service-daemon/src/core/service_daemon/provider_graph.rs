use std::any::TypeId;
use std::collections::{HashMap, HashSet, VecDeque};

use petgraph::{
    algo::toposort,
    graph::{DiGraph, NodeIndex},
};

use crate::core::context::__run_daemon_resources_scope;
use crate::core::provider_init::{
    ProviderInitBoundaryContext, ProviderInitBoundaryKind, ProviderInitFailure,
    ProviderInitSourceKind, ProviderRuntimePhase, provider_init_failure_into_error,
    with_provider_runtime_phase,
};
use crate::models::{PROVIDER_REGISTRY, ProviderEntry, ProviderInitError, ServiceDescription};

use super::ServiceDaemon;

impl ServiceDaemon {
    pub(super) async fn eager_init_reachable_providers(&self) -> Result<(), ProviderInitError> {
        let mut providers_by_id: HashMap<TypeId, &'static ProviderEntry> = HashMap::new();
        for entry in PROVIDER_REGISTRY.iter() {
            providers_by_id.insert(entry.type_id, entry);
        }

        // 1) Collect initial reachable set from service parameters.
        let mut reachable: HashSet<TypeId> = HashSet::new();
        let mut queue: VecDeque<TypeId> = VecDeque::new();
        for service in &self.services {
            for param in service.params() {
                if reachable.insert(param.type_id) {
                    queue.push_back(param.type_id);
                }
            }
        }

        // 2) Expand via provider->provider edges.
        while let Some(type_id) = queue.pop_front() {
            let Some(provider) = providers_by_id.get(&type_id) else {
                continue;
            };
            for dependency in provider.params {
                if reachable.insert(dependency.type_id) {
                    queue.push_back(dependency.type_id);
                }
            }
        }

        // 3) Filter eager targets.
        let eager_targets: Vec<&'static ProviderEntry> = reachable
            .iter()
            .filter_map(|type_id| providers_by_id.get(type_id).copied())
            .filter(|provider| provider.eager)
            .collect();

        if eager_targets.is_empty() {
            return Ok(());
        }

        // 4) Toposort reachable provider DAG to get a deterministic init order.
        // Nodes are provider TypeIds; edges are dep -> provider.
        let mut graph = DiGraph::<TypeId, ()>::new();
        let mut nodes: HashMap<TypeId, NodeIndex> = HashMap::new();

        for type_id in reachable.iter().copied() {
            if providers_by_id.contains_key(&type_id) {
                nodes
                    .entry(type_id)
                    .or_insert_with(|| graph.add_node(type_id));
            }
        }

        for (&type_id, entry) in providers_by_id.iter() {
            if !reachable.contains(&type_id) {
                continue;
            }
            let Some(&provider_node) = nodes.get(&type_id) else {
                continue;
            };
            for dependency in entry.params {
                if !reachable.contains(&dependency.type_id) {
                    continue;
                }
                if let Some(&dependency_node) = nodes.get(&dependency.type_id) {
                    graph.add_edge(dependency_node, provider_node, ());
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
                    .find_map(|(type_id, provider)| {
                        (*type_id == graph[err.node_id()]).then_some(provider.name)
                    })
                    .unwrap_or("<unknown>");
                return Err(provider_init_failure_into_error(
                    ProviderInitBoundaryContext::with_phase(
                        offending,
                        ProviderRuntimePhase::FrameworkValidation,
                        ProviderInitBoundaryKind::FrameworkValidation,
                    ),
                    ProviderInitFailure::fatal(
                        offending,
                        "Circular provider dependency reached eager_init; \
                              this should have been caught by validate_dependency_graph"
                            .to_owned(),
                        ProviderInitSourceKind::FrameworkGraphValidation,
                    ),
                ));
            }
        };

        // 5) Execute init in order, only for eager providers.
        let eager_ids: HashSet<TypeId> = eager_targets
            .iter()
            .map(|provider| provider.type_id)
            .collect();
        for node in order {
            let type_id = graph[node];
            if !eager_ids.contains(&type_id) {
                continue;
            }
            let Some(entry) = providers_by_id.get(&type_id).copied() else {
                return Err(provider_init_failure_into_error(
                    ProviderInitBoundaryContext::with_phase(
                        "<unknown>",
                        ProviderRuntimePhase::StartupEagerInit,
                        ProviderInitBoundaryKind::FrameworkValidation,
                    ),
                    ProviderInitFailure::fatal(
                        "<unknown>",
                        "provider missing from eager initialization graph".to_owned(),
                        ProviderInitSourceKind::FrameworkEagerInit,
                    ),
                ));
            };
            let resources = self.resources.clone();
            __run_daemon_resources_scope(resources, || async {
                with_provider_runtime_phase(
                    ProviderRuntimePhase::StartupEagerInit,
                    (entry.init)(self.restart_policy, self.cancellation_token.clone()),
                )
                .await
            })
            .await?;
        }

        Ok(())
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
pub(super) fn validate_dependency_graph<'a>(
    services: &[ServiceDescription],
    providers: impl IntoIterator<Item = &'a ProviderEntry>,
) -> Result<(), ProviderInitError> {
    let providers: Vec<&ProviderEntry> = providers.into_iter().collect();

    let mut graph = DiGraph::<&str, ()>::new();
    let mut service_nodes: HashMap<&str, NodeIndex> = HashMap::new();
    let mut type_nodes: HashMap<TypeId, NodeIndex> = HashMap::new();

    // Phase 1: Service -> Provider edges (roots).
    for service in services {
        let service_node = *service_nodes
            .entry(service.name())
            .or_insert_with(|| graph.add_node(service.name()));

        for param in service.params() {
            let type_node = *type_nodes
                .entry(param.type_id)
                .or_insert_with(|| graph.add_node(param.type_name));
            graph.add_edge(service_node, type_node, ());
        }
    }

    // Phase 2: Provider -> Provider edges (cycle-bearing subgraph).
    for provider in &providers {
        let provider_node = *type_nodes
            .entry(provider.type_id)
            .or_insert_with(|| graph.add_node(provider.name));

        for param in provider.params {
            let dependency_node = *type_nodes
                .entry(param.type_id)
                .or_insert_with(|| graph.add_node(param.type_name));
            graph.add_edge(provider_node, dependency_node, ());
        }
    }

    match toposort(&graph, None) {
        Ok(_order) => {
            for service in services {
                if !service.params().is_empty() {
                    let dependency_names: Vec<&str> = service
                        .params()
                        .iter()
                        .map(|param| param.type_name)
                        .collect();
                    tracing::info!(
                        service = %service.name(),
                        dependencies = ?dependency_names,
                        "Service dependency edge"
                    );
                }
            }
            for provider in &providers {
                if !provider.params.is_empty() {
                    let dependency_names: Vec<&str> = provider
                        .params
                        .iter()
                        .map(|param| param.type_name)
                        .collect();
                    tracing::info!(
                        provider = %provider.name,
                        dependencies = ?dependency_names,
                        "Provider dependency edge"
                    );
                }
            }
            tracing::info!(
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
                .filter(|&node| {
                    graph.contains_edge(node, cycle_node.node_id())
                        || graph.contains_edge(cycle_node.node_id(), node)
                })
                .map(|node| graph[node])
                .collect();

            Err(provider_init_failure_into_error(
                ProviderInitBoundaryContext::with_phase(
                    cycle_label,
                    ProviderRuntimePhase::FrameworkValidation,
                    ProviderInitBoundaryKind::FrameworkValidation,
                ),
                ProviderInitFailure::fatal(
                    cycle_label,
                    format!(
                        "Circular dependency detected in provider dependency graph. \
                         Cycle involves '{cycle_label}', related nodes: {involved:?}. \
                         This would deadlock at runtime; review the #[provider] chain for these types."
                    ),
                    ProviderInitSourceKind::FrameworkGraphValidation,
                ),
            ))
        }
    }
}
