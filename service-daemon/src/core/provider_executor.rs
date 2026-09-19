use std::any::TypeId;
use std::collections::{HashMap, HashSet};

use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};
use tokio_util::sync::CancellationToken;

use crate::core::provider_init::{
    ProviderInitBoundaryContext, ProviderInitBoundaryKind, ProviderInitFailure,
    ProviderInitSourceKind, provider_init_failure_boundary, provider_init_failure_into_error,
};
use crate::core::provider_scope::ProviderCacheScope;
use crate::RestartPolicy;
use crate::models::{
    PROVIDER_CANDIDATE_REGISTRY, PROVIDER_REGISTRY, ProviderCandidateEntry,
    ProviderCandidateInitError, ProviderDependencyKind, ProviderEntry, ProviderError,
    ProviderInitError, ServiceParam,
};
use std::sync::Arc;
use tracing::{error, info, warn};

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

#[doc(hidden)]
pub async fn resolve_provider_contract<T>(
    contract_name: &'static str,
    policy: RestartPolicy,
    cancel: CancellationToken,
) -> Result<Arc<T>, ProviderInitFailure>
where
    T: 'static + Send + Sync + Clone,
{
    resolve_provider_contract_inner::<T>(contract_name, policy, cancel).await
}

#[doc(hidden)]
pub async fn resolve_provider_contract_managed<T>(
    contract_name: &'static str,
    policy: RestartPolicy,
    cancel: CancellationToken,
) -> Result<Arc<T>, ProviderError>
where
    T: 'static + Send + Sync + Clone,
{
    resolve_provider_contract_inner::<T>(contract_name, policy, cancel)
        .await
        .map_err(|failure| ProviderError::Fatal(failure.error().to_string()))
}

async fn resolve_provider_contract_inner<T>(
    contract_name: &'static str,
    policy: RestartPolicy,
    cancel: CancellationToken,
) -> Result<Arc<T>, ProviderInitFailure>
where
    T: 'static + Send + Sync + Clone,
{
    let output_type_id = TypeId::of::<T>();
    let mut candidates: Vec<&'static ProviderCandidateEntry> = PROVIDER_CANDIDATE_REGISTRY
        .iter()
        .filter(|candidate| candidate.output_type_id == output_type_id)
        .collect();

    candidates.sort_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.module.cmp(right.module))
            .then_with(|| left.name.cmp(right.name))
    });

    if candidates.is_empty() {
        let message = format!(
            "provider contract `{contract_name}` has no registered #[provider_impl] candidates"
        );
        error!(provider = contract_name, "{message}");
        return Err(ProviderInitFailure::fatal(
            contract_name,
            message,
            ProviderInitSourceKind::UserProviderFatal,
        ));
    }

    let mut last_non_fatal: Option<String> = None;
    for candidate in candidates {
        if cancel.is_cancelled() {
            return Err(ProviderInitFailure::cancelled(candidate.name));
        }

        if let Err(error) = prepare_provider_params(candidate.params, policy, cancel.clone()).await {
            match error {
                ProviderInitError::Timeout {
                    provider,
                    timeout,
                    last_error,
                } => {
                    let message = format!(
                        "candidate `{}` dependency `{provider}` timed out after {:?}: {last_error}",
                        candidate.name, timeout
                    );
                    warn!(
                        provider = contract_name,
                        candidate = candidate.name,
                        "{message}"
                    );
                    last_non_fatal = Some(message);
                    continue;
                }
                ProviderInitError::Fatal { .. } | ProviderInitError::Cancelled { .. } => {
                    return Err(ProviderInitFailure::new(
                        ProviderInitSourceKind::DependencyProvider,
                        error,
                    ));
                }
            }
        }

        match (candidate.init)(policy, cancel.clone()).await {
            Ok(value) => match value.downcast::<T>() {
                Ok(value) => {
                    info!(
                        provider = contract_name,
                        candidate = candidate.name,
                        priority = candidate.priority,
                        "Provider contract candidate selected"
                    );
                    return Ok(value);
                }
                Err(_) => {
                    return Err(ProviderInitFailure::fatal(
                        candidate.name,
                        format!(
                            "provider candidate returned a value that does not match contract `{contract_name}`"
                        ),
                        ProviderInitSourceKind::UserProviderFatal,
                    ));
                }
            },
            Err(ProviderCandidateInitError::Unavailable(message)) => {
                let message = format!("candidate `{}` unavailable: {message}", candidate.name);
                info!(
                    provider = contract_name,
                    candidate = candidate.name,
                    "{message}"
                );
                last_non_fatal = Some(message);
            }
            Err(ProviderCandidateInitError::Failed(failure))
                if matches!(failure.error(), ProviderInitError::Timeout { .. }) =>
            {
                let error = provider_init_failure_into_error(
                    ProviderInitBoundaryContext::new(
                        candidate.name,
                        ProviderInitBoundaryKind::SnapshotResolve,
                    ),
                    (*failure).clone(),
                );
                let message = format!("candidate `{}` timed out: {error}", candidate.name);
                warn!(
                    provider = contract_name,
                    candidate = candidate.name,
                    "{message}"
                );
                last_non_fatal = Some(message);
            }
            Err(ProviderCandidateInitError::Failed(failure)) => {
                return Err(*failure);
            }
        }
    }

    let message = match last_non_fatal {
        Some(message) => format!(
            "provider contract `{contract_name}` exhausted all #[provider_impl] candidates; last result: {message}"
        ),
        None => format!("provider contract `{contract_name}` exhausted all #[provider_impl] candidates"),
    };
    error!(provider = contract_name, "{message}");
    Err(ProviderInitFailure::fatal(
        contract_name,
        message,
        ProviderInitSourceKind::UserProviderFatal,
    ))
}

#[doc(hidden)]
pub fn provider_contract_cache_scope(output_type_id: TypeId) -> ProviderCacheScope {
    if PROVIDER_CANDIDATE_REGISTRY.iter().any(|candidate| {
        candidate.output_type_id == output_type_id
            && candidate.cache_scope == ProviderCacheScope::DaemonLocal
    }) {
        ProviderCacheScope::DaemonLocal
    } else {
        ProviderCacheScope::Inherited
    }
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
