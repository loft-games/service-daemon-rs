use std::any::TypeId;
use std::collections::{HashMap, HashSet};

use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};
use tokio_util::sync::CancellationToken;

use crate::core::provider_init::{
    ProviderInitBoundaryContext, ProviderInitBoundaryKind, ProviderInitFailure,
    ProviderInitSourceKind, provider_init_failure_boundary,
};
use crate::RestartPolicy;
use crate::models::{
    PROVIDER_REGISTRY, ProviderDependencyKind, ProviderEntry, ProviderInitError, ServiceParam,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProviderDemand {
    Snapshot,
    RwLock,
    Mutex,
}

impl ProviderDemand {
    fn from_kind(kind: ProviderDependencyKind) -> Self {
        match kind {
            ProviderDependencyKind::Snapshot => Self::Snapshot,
            ProviderDependencyKind::RwLock => Self::RwLock,
            ProviderDependencyKind::Mutex => Self::Mutex,
        }
    }

    fn merge(self, other: Self) -> Self {
        use ProviderDemand::{Mutex, RwLock, Snapshot};
        match (self, other) {
            (Mutex, _) | (_, Mutex) => Mutex,
            (RwLock, _) | (_, RwLock) => RwLock,
            (Snapshot, Snapshot) => Snapshot,
        }
    }
}

pub async fn prepare_provider_params(
    params: &[ServiceParam],
    policy: RestartPolicy,
    cancel: CancellationToken,
) -> Result<(), ProviderInitError> {
    prepare_provider_params_inner(params, policy, cancel, false).await
}

pub async fn prepare_eager_provider_params(
    params: &[ServiceParam],
    policy: RestartPolicy,
    cancel: CancellationToken,
) -> Result<(), ProviderInitError> {
    prepare_provider_params_inner(params, policy, cancel, true).await
}

async fn prepare_provider_params_inner(
    params: &[ServiceParam],
    policy: RestartPolicy,
    cancel: CancellationToken,
    eager: bool,
) -> Result<(), ProviderInitError> {
    let providers_by_id = providers_by_id()?;
    let mut demand_by_id: HashMap<TypeId, ProviderDemand> = HashMap::new();
    let mut dependent_by_dependency: HashMap<TypeId, TypeId> = HashMap::new();
    let root_ids: HashSet<TypeId> = params.iter().map(|param| param.type_id).collect();
    let mut reachable: HashSet<TypeId> = HashSet::new();
    let mut stack = Vec::new();

    for param in params {
        let demand = ProviderDemand::from_kind(param.kind);
        merge_demand(&mut demand_by_id, param.type_id, demand);
        stack.push(param.type_id);
    }

    while let Some(type_id) = stack.pop() {
        if !reachable.insert(type_id) {
            continue;
        }
        let provider = providers_by_id.get(&type_id).copied().ok_or_else(|| {
            missing_provider_error("<dependency graph>", type_id, "provider is not registered")
        })?;
        for dependency in provider.params {
            dependent_by_dependency
                .entry(dependency.type_id)
                .or_insert(type_id);
            merge_demand(
                &mut demand_by_id,
                dependency.type_id,
                ProviderDemand::from_kind(dependency.kind),
            );
            stack.push(dependency.type_id);
        }
    }

    if reachable.is_empty() {
        return Ok(());
    }

    let mut graph = DiGraph::<TypeId, ()>::new();
    let mut nodes = HashMap::<TypeId, NodeIndex>::new();

    for type_id in reachable.iter().copied() {
        nodes
            .entry(type_id)
            .or_insert_with(|| graph.add_node(type_id));
    }

    for type_id in reachable.iter().copied() {
        let provider = providers_by_id.get(&type_id).copied().ok_or_else(|| {
            missing_provider_error("<dependency graph>", type_id, "provider is not registered")
        })?;
        let provider_node = nodes[&type_id];
        for dependency in provider.params {
            if !reachable.contains(&dependency.type_id) {
                continue;
            }
            let dependency_node = nodes[&dependency.type_id];
            graph.add_edge(dependency_node, provider_node, ());
        }
    }

    let order = toposort(&graph, None).map_err(|err| {
        let type_id = graph[err.node_id()];
        missing_provider_error(
            "<dependency graph>",
            type_id,
            "circular provider dependency reached graph executor",
        )
    })?;

    for node in order {
        if cancel.is_cancelled() {
            return Err(ProviderInitError::Cancelled {
                provider: "<dependency graph>".to_owned(),
            });
        }
        let type_id = graph[node];
        let provider = providers_by_id.get(&type_id).copied().ok_or_else(|| {
            missing_provider_error("<dependency graph>", type_id, "provider is not registered")
        })?;
        let demand = demand_by_id
            .get(&type_id)
            .copied()
            .unwrap_or(ProviderDemand::Snapshot);
        let result = match demand {
            ProviderDemand::Snapshot if eager => {
                (provider.init_eager)(policy, cancel.clone()).await
            }
            ProviderDemand::Snapshot => (provider.init)(policy, cancel.clone()).await,
            ProviderDemand::RwLock => (provider.init_rwlock)(policy, cancel.clone()).await,
            ProviderDemand::Mutex => (provider.init_mutex)(policy, cancel.clone()).await,
        };

        if let Err(error) = result {
            if !root_ids.contains(&type_id)
                && let Some(dependent_type_id) = dependent_by_dependency.get(&type_id)
                && let Some(dependent) = providers_by_id.get(dependent_type_id).copied()
            {
                let context = ProviderInitBoundaryContext::new(
                    dependent.name,
                    boundary_kind_for_demand(
                        demand_by_id
                            .get(dependent_type_id)
                            .copied()
                            .unwrap_or(ProviderDemand::Snapshot),
                    ),
                );
                return provider_init_failure_boundary(
                    context,
                    Err(ProviderInitFailure::new(
                        ProviderInitSourceKind::DependencyProvider,
                        error,
                    )),
                );
            }
            return Err(error);
        }
    }

    Ok(())
}

fn boundary_kind_for_demand(demand: ProviderDemand) -> ProviderInitBoundaryKind {
    match demand {
        ProviderDemand::Snapshot => ProviderInitBoundaryKind::SnapshotResolve,
        ProviderDemand::RwLock => ProviderInitBoundaryKind::RwLockResolve,
        ProviderDemand::Mutex => ProviderInitBoundaryKind::MutexResolve,
    }
}

pub async fn prepare_provider_type(
    type_id: TypeId,
    kind: ProviderDependencyKind,
    policy: RestartPolicy,
    cancel: CancellationToken,
) -> Result<(), ProviderInitError> {
    let params = [ServiceParam {
        name: "<direct>",
        type_name: "<direct>",
        type_id,
        kind,
    }];
    prepare_provider_params(&params, policy, cancel).await
}

fn providers_by_id() -> Result<HashMap<TypeId, &'static ProviderEntry>, ProviderInitError> {
    let mut providers = HashMap::new();
    for entry in PROVIDER_REGISTRY.iter() {
        if let Some(previous) = providers.insert(entry.type_id, entry) {
            return Err(ProviderInitError::Fatal {
                provider: entry.name.to_owned(),
                message: format!(
                    "duplicate provider registration for type `{}`; previous registration was `{}`",
                    entry.name, previous.name
                ),
            });
        }
    }
    Ok(providers)
}

fn merge_demand(
    demand_by_id: &mut HashMap<TypeId, ProviderDemand>,
    type_id: TypeId,
    demand: ProviderDemand,
) {
    demand_by_id
        .entry(type_id)
        .and_modify(|existing| *existing = existing.merge(demand))
        .or_insert(demand);
}

fn missing_provider_error(
    provider: &'static str,
    type_id: TypeId,
    reason: &'static str,
) -> ProviderInitError {
    ProviderInitError::Fatal {
        provider: provider.to_owned(),
        message: format!("{reason}: {type_id:?}. Add #[provider] for this type."),
    }
}
