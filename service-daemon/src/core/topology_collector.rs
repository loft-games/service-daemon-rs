//! Runtime behavioral topology collector.
//!
//! Gated behind the `diagnostics` feature, this module subscribes to the
//! [`LogQueue`](super::logging::LogQueue) broadcast channel and aggregates
//! causal edges between services based on natively propagated `source_service_id`.
//!
//! # Architecture
//!
//! The collector runs as a background tokio task (spawned via
//! [`start_topology_collector`]) and builds a directed acyclic graph (DAG)
//! of service interactions. Each edge represents "service A triggered
//! service B".
//!
//! This collector is **stateless**: it relies on the causal identity
//! (`source_service_id`) injected by the `TriggerRunner` and propagated
//! through the logging pipeline.
//!
//! # Data Flow
//!
//! ```text
//! LogQueue (broadcast)
//!     |
//!     v
//! TopologyCollector (subscriber)
//!     |  extracts (service_id, source_service_id)
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

use crate::models::{SERVICE_REGISTRY, ServiceId};

use super::logging::{LogEvent, get_log_queue};

// ---------------------------------------------------------------------------
// Edge model
// ---------------------------------------------------------------------------

/// A directed edge in the behavioral topology graph.
///
/// Represents a causal relationship: `source` service triggered `target`
/// service. The `count` field tracks how many times this edge was observed.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Ord, PartialOrd)]
struct Edge {
    /// The `ServiceId` of the emitter (the service that published the signal).
    source: ServiceId,
    /// The `ServiceId` of the consumer (the trigger that reacted).
    target: ServiceId,
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
/// `source_service_id`. If it does, a causal relationship is established.
fn process_event(state: &Arc<RwLock<TopologyState>>, event: &LogEvent) {
    // We only care about events that have both a target (service_id)
    // and a known source (source_service_id).
    let target = match event.service_id {
        Some(id) => id,
        None => return,
    };

    let source = match event.source_service_id {
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

    if guard.edges.is_empty() {
        return None;
    }

    let mut lines = vec!["graph LR".to_string()];

    // Sort edges for deterministic output
    let mut sorted_edges: Vec<_> = guard.edges.iter().collect();
    sorted_edges.sort_by(|(a, _), (b, _)| a.cmp(b));

    for (edge, count) in sorted_edges {
        let source_id = edge.source;
        let target_id = edge.target;

        // Map IDs to names using the global static registry
        let source_name = SERVICE_REGISTRY
            .get(source_id.value())
            .map(|e| e.name)
            .unwrap_or("unknown");
        let target_name = SERVICE_REGISTRY
            .get(target_id.value())
            .map(|e| e.name)
            .unwrap_or("unknown");

        let source_node = format!("{}_{}", source_name, source_id.value());
        let target_node = format!("{}_{}", target_name, target_id.value());

        lines.push(format!(
            "    {}[\"{}\"] -->|{}x| {}[\"{}\"]",
            source_node, source_name, count, target_node, target_name
        ));
    }

    Some(lines.join("\n"))
}

pub fn reset_topology() {
    if let Some(state) = TOPOLOGY_STATE.get() {
        if let Ok(mut guard) = state.write() {
            guard.edges.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::borrow::Cow;
    use uuid::Uuid;

    use crate::core::logging::LogLevel;

    #[test]
    fn test_stateless_correlation() {
        let state = Arc::new(RwLock::new(TopologyState::default()));

        // Event with both service_id and source_service_id
        let event = LogEvent {
            timestamp: Utc::now(),
            level: LogLevel::Info,
            target: Cow::Borrowed("test"),
            message: "trigger fired".to_string(),
            module_path: None,
            file: None,
            line: None,
            service_id: Some(ServiceId::new(2)),
            source_service_id: Some(ServiceId::new(1)),
            message_id: Some(Uuid::now_v7()),
            instance_id: None,
            error_chain: None,
        };
        process_event(&state, &event);

        let guard = state.read().unwrap();
        let edge = Edge {
            source: ServiceId::new(1),
            target: ServiceId::new(2),
        };
        assert_eq!(guard.edges.get(&edge), Some(&1));
    }
}
