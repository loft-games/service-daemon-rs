use std::any::TypeId;
use std::collections::{HashMap, HashSet, VecDeque};

use petgraph::{
    algo::toposort,
    graph::{DiGraph, NodeIndex},
};

use crate::core::context::__run_daemon_resources_scope;
use crate::core::provider_executor::prepare_eager_provider_params;
use crate::core::provider_init::{
    ProviderInitBoundaryContext, ProviderInitBoundaryKind, ProviderInitFailure,
    ProviderInitSourceKind, ProviderRuntimePhase, provider_init_failure_into_error,
    with_provider_runtime_phase,
};
use crate::models::{
    PROVIDER_CANDIDATE_REGISTRY, PROVIDER_REGISTRY, ProviderDependencyKind, ProviderEntry,
    ProviderInitError, ServiceDescription, ServiceParam,
};

use super::DaemonInstanceInner;

impl DaemonInstanceInner {
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

        let eager_params: Vec<ServiceParam> = eager_targets
            .iter()
            .map(|provider| ServiceParam {
                name: "<eager>",
                type_name: provider.name,
                type_id: provider.type_id,
                kind: ProviderDependencyKind::Snapshot,
            })
            .collect();

        let resources = self.resources.clone();
        __run_daemon_resources_scope(resources, || async {
            with_provider_runtime_phase(
                ProviderRuntimePhase::StartupEagerInit,
                prepare_eager_provider_params(
                    &eager_params,
                    self.restart_policy,
                    self.cancellation_token.clone(),
                ),
            )
            .await
        })
        .await?;

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
    validate_dependency_graph_with_candidates(
        services,
        providers,
        PROVIDER_CANDIDATE_REGISTRY.iter(),
    )
}

fn validate_dependency_graph_with_candidates<'a, 'b>(
    services: &[ServiceDescription],
    providers: impl IntoIterator<Item = &'a ProviderEntry>,
    candidates: impl IntoIterator<Item = &'b crate::models::ProviderCandidateEntry>,
) -> Result<(), ProviderInitError> {
    let providers: Vec<&ProviderEntry> = providers.into_iter().collect();
    let candidates: Vec<&crate::models::ProviderCandidateEntry> =
        candidates.into_iter().collect();

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

        for param in structural_provider_dependencies(provider, &candidates) {
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
                let params = structural_provider_dependencies(provider, &candidates);
                if !params.is_empty() {
                    let dependency_names: Vec<&str> =
                        params.iter().map(|param| param.type_name).collect();
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

fn structural_provider_dependencies(
    provider: &ProviderEntry,
    candidates: &[&crate::models::ProviderCandidateEntry],
) -> Vec<&'static ServiceParam> {
    let mut params: Vec<&'static ServiceParam> = provider.params.iter().collect();
    params.extend(
        candidates
            .iter()
            .filter(|candidate| candidate.output_type_id == provider.type_id)
            .flat_map(|candidate| candidate.params.iter()),
    );
    params
}

#[cfg(test)]
mod tests {
    use super::validate_dependency_graph_with_candidates;
    use crate::core::provider_init::ProviderInitFailure;
    use crate::core::provider_scope::ProviderCacheScope;
    use crate::models::{
        ProviderCandidateEntry, ProviderCandidateInitError, ProviderDependencyKind,
        ProviderEntry, ProviderInitError, RestartPolicy, ServiceParam,
    };
    use futures::future::BoxFuture;
    use std::any::{Any, TypeId};
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    #[derive(Clone)]
    struct CandidateCycleA;

    #[derive(Clone)]
    struct CandidateCycleB;

    struct CandidateCycleIdentity;

    fn provider_init(
        _policy: RestartPolicy,
        _cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<(), ProviderInitError>> {
        Box::pin(async { Ok(()) })
    }

    fn candidate_init(
        _policy: RestartPolicy,
        _cancel: CancellationToken,
    ) -> BoxFuture<
        'static,
        Result<Arc<dyn Any + Send + Sync>, ProviderCandidateInitError>,
    > {
        Box::pin(async {
            Err(ProviderCandidateInitError::Failed(Box::new(
                ProviderInitFailure::fatal(
                    "candidate_cycle",
                    "not executed by graph validation".to_owned(),
                    crate::core::provider_init::ProviderInitSourceKind::UserProviderFatal,
                ),
            )))
        })
    }

    #[test]
    fn candidate_dependency_edges_still_participate_in_cycle_detection() {
        let a_dependency: &'static [ServiceParam] = Box::leak(
            vec![ServiceParam {
                name: "a",
                type_name: "CandidateCycleA",
                type_id: TypeId::of::<CandidateCycleA>(),
                kind: ProviderDependencyKind::Snapshot,
            }]
            .into_boxed_slice(),
        );
        let b_dependency: &'static [ServiceParam] = Box::leak(
            vec![ServiceParam {
                name: "b",
                type_name: "CandidateCycleB",
                type_id: TypeId::of::<CandidateCycleB>(),
                kind: ProviderDependencyKind::Snapshot,
            }]
            .into_boxed_slice(),
        );
        let contract = ProviderEntry {
            name: "CandidateCycleA",
            module: module_path!(),
            type_id: TypeId::of::<CandidateCycleA>(),
            params: &[],
            eager: false,
            init: provider_init,
            init_eager: provider_init,
            init_rwlock: provider_init,
            init_mutex: provider_init,
        };
        let dependency = ProviderEntry {
            name: "CandidateCycleB",
            module: module_path!(),
            type_id: TypeId::of::<CandidateCycleB>(),
            params: a_dependency,
            eager: false,
            init: provider_init,
            init_eager: provider_init,
            init_rwlock: provider_init,
            init_mutex: provider_init,
        };
        let candidate = ProviderCandidateEntry {
            name: "candidate_cycle_impl",
            module: module_path!(),
            output_type_id: TypeId::of::<CandidateCycleA>(),
            output_type_name: "CandidateCycleA",
            provider_type_id: TypeId::of::<CandidateCycleIdentity>(),
            priority: 50,
            params: b_dependency,
            cache_scope: ProviderCacheScope::Inherited,
            init: candidate_init,
        };

        let error = validate_dependency_graph_with_candidates(
            &[],
            [&contract, &dependency],
            [&candidate],
        )
        .expect_err("candidate dependency edge should complete the provider cycle");

        assert!(matches!(error, ProviderInitError::Fatal { .. }));
    }
}
