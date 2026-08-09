//! Runtime behavioral topology collector.
//!
//! Gated behind the `diagnostics` feature, this module subscribes to the
//! [`LogQueue`](super::logging::model::LogQueue) broadcast channel and aggregates
//! causal edges between services based on natively propagated `source_service_instance_id`.
//!
//! # Architecture
//!
//! The collector runs as a background tokio task (spawned via
//! [`start_topology_collector`]) and builds a directed acyclic graph (DAG)
//! of service interactions. Each edge represents "service A triggered
//! service B".
//!
//! This collector is **stateless**: it relies on the causal identity
//! (`source_service_instance_id`) injected by the `TriggerRunner` and propagated
//! through the logging pipeline.
//!
//! # Data Flow
//!
//! ```text
//! LogQueue (broadcast)
//!     |
//!     v
//! TopologyCollector (subscriber)
//!     |  extracts (service_instance_id, source_service_instance_id)
//!     |  records edge (source -> service)
//!     v
//! EdgeMap: HashMap<(source, target), count>
//!     |
//!     v
//! export_mermaid() -> Mermaid DAG string
//! ```

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::models::ServiceInstanceId;

use super::logging::model::{LogEvent, get_log_queue};

// ---------------------------------------------------------------------------
// Edge model
// ---------------------------------------------------------------------------

/// A directed edge in the behavioral topology graph.
///
/// Represents a causal relationship: `source` service triggered `target`
/// service. The `count` field tracks how many times this edge was observed.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Ord, PartialOrd)]
struct Edge {
    /// The `ServiceInstanceId` of the emitter (the service that published the signal).
    source: ServiceInstanceId,
    /// The `ServiceInstanceId` of the consumer (the trigger that reacted).
    target: ServiceInstanceId,
}

/// Thread-safe storage for the accumulated topology edges.
#[derive(Default)]
struct TopologyState {
    /// Edge -> observation count.
    edges: HashMap<Edge, u64>,
}

/// Global topology state, initialized on first collector start.
static TOPOLOGY_STATE: OnceLock<Arc<RwLock<TopologyState>>> = OnceLock::new();

/// Gets or initializes the global topology state.
fn get_state() -> &'static Arc<RwLock<TopologyState>> {
    TOPOLOGY_STATE.get_or_init(|| Arc::new(RwLock::new(TopologyState::default())))
}

// ---------------------------------------------------------------------------
// Collector task
// ---------------------------------------------------------------------------

/// Starts the topology collector as a background tokio task.
///
/// This function is idempotent and will only start the task once per process.
pub fn start_topology_collector() -> JoinHandle<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static STARTED: AtomicBool = AtomicBool::new(false);

    if STARTED.swap(true, Ordering::SeqCst) {
        return tokio::spawn(async {}); // Already started, return dummy handle
    }

    let state = get_state().clone();
    let mut rx = get_log_queue().tx.subscribe();

    tokio::spawn(async move {
        debug!("Topology collector (stateless) started");

        loop {
            match rx.recv().await {
                Ok(event) => {
                    process_event(&state, &event);
                }
                Err(RecvError::Lagged(n)) => {
                    warn!(
                        skipped = n,
                        "Topology collector lagged, {} events dropped", n
                    );
                }
                Err(RecvError::Closed) => {
                    debug!("Topology collector: LogQueue closed, shutting down");
                    break;
                }
            }
        }
    })
}

/// Processes a single log event to extract causal edges.
///
/// Under the stateless model, we simply check if the event carries a
/// `source_service_instance_id`. If it does, a causal relationship is established.
fn process_event(state: &Arc<RwLock<TopologyState>>, event: &LogEvent) {
    // We only care about events that have both a target (service_instance_id)
    // and a known source (source_service_instance_id).
    let target = match event.service_instance_id {
        Some(id) => id,
        None => return,
    };

    let source = match event.source_service_instance_id {
        Some(id) => id,
        None => return,
    };

    // Avoid self-loops (noise filtering)
    if source == target {
        return;
    }

    let mut guard = match state.write() {
        Ok(g) => g,
        Err(_) => return,
    };

    let edge = Edge { source, target };
    *guard.edges.entry(edge).or_insert(0) += 1;
}

// ---------------------------------------------------------------------------
// Export API
// ---------------------------------------------------------------------------

/// Exports the accumulated behavioral topology as a Mermaid flowchart string.
pub fn export_mermaid() -> Option<String> {
    let state = get_state();
    let guard = state.read().ok()?;

    render_mermaid_edges(&guard.edges)
}

fn render_mermaid_edges(edges: &HashMap<Edge, u64>) -> Option<String> {
    if edges.is_empty() {
        return None;
    }

    let mut lines = vec!["graph LR".to_string()];

    // Sort edges for deterministic output
    let mut sorted_edges: Vec<_> = edges.iter().collect();
    sorted_edges.sort_by_key(|(edge, _)| *edge);

    for (edge, count) in sorted_edges {
        let source_id = edge.source;
        let target_id = edge.target;

        let source_label = source_id.to_string();
        let target_label = target_id.to_string();
        let source_node = format!("svc_{}", source_id.as_uuid().simple());
        let target_node = format!("svc_{}", target_id.as_uuid().simple());

        lines.push(format!(
            "    {}[\"{}\"] -->|{}x| {}[\"{}\"]",
            source_node, source_label, count, target_node, target_label
        ));
    }

    Some(lines.join("\n"))
}

pub fn reset_topology() {
    if let Some(state) = TOPOLOGY_STATE.get()
        && let Ok(mut guard) = state.write()
    {
        guard.edges.clear();
    }
}

#[cfg(test)]
pub(crate) fn record_topology_edge_for_test(source: ServiceInstanceId, target: ServiceInstanceId) {
    let state = get_state().clone();
    let mut guard = state
        .write()
        .unwrap_or_else(|err| panic!("topology state lock poisoned: {err}"));
    *guard.edges.entry(Edge { source, target }).or_insert(0) += 1;
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::borrow::Cow;
    use uuid::Uuid;

    use crate::core::logging::model::LogLevel;

    #[test]
    fn test_stateless_correlation() {
        let state = Arc::new(RwLock::new(TopologyState::default()));

        // Event with both service_instance_id and source_service_instance_id
        let event = LogEvent {
            timestamp: Utc::now(),
            level: LogLevel::Info,
            target: Cow::Borrowed("test"),
            message: "trigger fired".to_string(),
            module_path: None,
            file: None,
            line: None,
            service_instance_id: Some(ServiceInstanceId::new(uuid::Uuid::from_u128(2))),
            source_service_instance_id: Some(ServiceInstanceId::new(uuid::Uuid::from_u128(1))),
            message_id: Some(Uuid::now_v7()),
            trigger_instance_id: None,
            error_chain: None,
        };
        process_event(&state, &event);

        let guard = state.read().unwrap();
        let edge = Edge {
            source: ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            target: ServiceInstanceId::new(uuid::Uuid::from_u128(2)),
        };
        assert_eq!(guard.edges.get(&edge), Some(&1));
    }

    #[test]
    fn export_mermaid_uses_uuid_instance_ids_without_registry_index_lookup() {
        let source = ServiceInstanceId::new(
            Uuid::parse_str("019fe746-6158-7403-82c9-ac1111111111").unwrap(),
        );
        let target = ServiceInstanceId::new(
            Uuid::parse_str("019fe746-6158-7403-82c9-ac2222222222").unwrap(),
        );
        let mut edges = HashMap::new();
        edges.insert(Edge { source, target }, 1);

        let mermaid = render_mermaid_edges(&edges).expect("topology should contain the local edge");
        assert!(mermaid.contains(&format!("svc_{}", source.as_uuid().simple())));
        assert!(mermaid.contains(&format!("svc_{}", target.as_uuid().simple())));
        assert!(mermaid.contains(&format!("[\"{source}\"]")));
        assert!(mermaid.contains(&format!("[\"{target}\"]")));
        assert!(
            !mermaid.contains("unknown"),
            "topology export should not treat instance IDs as registry indexes"
        );
    }
}
