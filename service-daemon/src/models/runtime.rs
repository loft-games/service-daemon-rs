//! Owned runtime snapshot types.
//!
//! These read models copy framework facts out of the daemon. They never expose
//! runtime locks, semaphores, or control handles.

use std::time::Duration;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::service::{ServiceId, ServiceScheduling, ServiceStatus};

/// Daemon-level runtime facts.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonRuntimeSnapshot {
    /// Stable identifier for this daemon resource instance.
    pub daemon_id: Uuid,
    /// Creation time of the daemon resources.
    pub start_time: DateTime<Utc>,
    /// Monotonic elapsed time since creation.
    pub uptime: Duration,
    /// Whether daemon shutdown has been requested.
    pub shutdown_requested: bool,
    /// Number of services registered in this runtime-facts store.
    pub service_count: usize,
    /// Number of triggers observed by a `TriggerRunner`.
    pub trigger_count: usize,
    /// Snapshot generation time.
    pub generated_at: DateTime<Utc>,
}

/// Runtime facts for one managed service.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRuntimeSnapshot {
    /// Registry identity.
    pub service_id: ServiceId,
    pub service_name: &'static str,
    pub priority: u8,
    pub declared_scheduling: ServiceScheduling,
    /// Current lifecycle status from the daemon status plane.
    pub status: ServiceStatus,
    /// Latest generation number observed by the supervisor.
    pub generation: u64,
    /// Restarts scheduled after generation exits.
    pub restart_count: u64,
    /// Lifecycle timeline for the latest observed generation.
    pub last_started_at: Option<DateTime<Utc>>,
    pub last_stopped_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub current_backoff: Option<Duration>,
    pub healthy_since: Option<DateTime<Utc>>,
}

/// Error summary carried by a facts-only readiness snapshot.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessServiceError {
    /// Service that produced the error.
    pub service_id: ServiceId,
    pub service_name: &'static str,
    pub status: ServiceStatus,
    /// Human-readable error summary.
    pub message: String,
}

/// Facts-only readiness grouping for services.
///
/// This type deliberately does not provide an `is_ready` or degraded verdict.
/// Applications decide platform-specific readiness semantics from these facts.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessSnapshot {
    /// Services currently in `Healthy`.
    pub healthy: Vec<ServiceRuntimeSnapshot>,
    /// Services currently in `Initializing`.
    pub initializing: Vec<ServiceRuntimeSnapshot>,
    /// Services currently in `Recovering`.
    pub recovering: Vec<ServiceRuntimeSnapshot>,
    /// Services currently in `Restoring`.
    pub restoring: Vec<ServiceRuntimeSnapshot>,
    /// Services currently in `NeedReload`.
    pub need_reload: Vec<ServiceRuntimeSnapshot>,
    /// Services currently in `ShuttingDown`.
    pub shutting_down: Vec<ServiceRuntimeSnapshot>,
    /// Services currently in `Terminated`.
    pub terminated: Vec<ServiceRuntimeSnapshot>,
    /// Recent daemon-observed service error summaries.
    pub recent_errors: Vec<ReadinessServiceError>,
    /// Wall-clock time when this snapshot was generated.
    pub generated_at: DateTime<Utc>,
}

/// Pressure and dispatch facts for one trigger service.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerPressureSnapshot {
    /// Trigger service identity.
    pub service_id: ServiceId,
    pub service_name: &'static str,
    pub generation: u64,
    /// Currently running handler dispatches.
    pub in_flight: usize,
    /// Current framework-owned concurrency limit.
    pub current_limit: usize,
    /// Currently available dispatch permits.
    pub available_permits: usize,
    /// Dispatch and retry counters recorded by the runner/interceptors.
    pub dispatched_total: u64,
    pub completed_total: u64,
    pub failed_total: u64,
    pub retry_total: u64,
    /// Recent dispatch timeline.
    pub last_event_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    /// Optional streaming pressure counters, when the host can report them.
    pub lagged_count: Option<u64>,
    pub dropped_count: Option<u64>,
    pub closed_count: Option<u64>,
    pub backpressure_count: Option<u64>,
}

/// Runtime facts for one trigger service.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerRuntimeSnapshot {
    /// Trigger service identity.
    pub service_id: ServiceId,
    pub service_name: &'static str,
    pub generation: u64,
    /// Pressure facts observed by the trigger runner.
    pub pressure: TriggerPressureSnapshot,
}
