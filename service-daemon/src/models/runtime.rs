//! Owned runtime snapshot types.
//!
//! These read models copy framework facts out of the daemon. They never expose
//! runtime locks, semaphores, or control handles.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::service::{ServiceInstanceId, ServiceScheduling, ServiceStatus};

/// Runtime identity for one daemon instance in this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "file-logging", derive(serde::Serialize, serde::Deserialize))]
pub struct DaemonInstanceId(Uuid);

impl DaemonInstanceId {
    /// Explicitly construct a daemon instance id.
    #[inline]
    pub const fn new(id: Uuid) -> Self {
        Self(id)
    }

    /// Allocate a new UUIDv7 daemon instance id.
    #[inline]
    pub fn new_v7() -> Self {
        Self(Uuid::now_v7())
    }

    /// Return the underlying UUID.
    #[inline]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for DaemonInstanceId {
    fn default() -> Self {
        Self(Uuid::nil())
    }
}

impl FromStr for DaemonInstanceId {
    type Err = uuid::Error;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let raw = input.strip_prefix("daemon#").unwrap_or(input);
        Uuid::parse_str(raw).map(Self)
    }
}

impl fmt::Display for DaemonInstanceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "daemon#{}", self.0)
    }
}

/// Runtime identity for one framework-owned HighPriority runtime shard.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HighPriorityShardId(pub u64);

impl fmt::Display for HighPriorityShardId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "hp#{}", self.0)
    }
}

/// Best-effort pressure state for a HighPriority runtime shard.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HighPriorityShardPressureState {
    /// The shard has not collected enough observations for a stronger state.
    Unknown,
    /// The shard has enough samples and does not currently show pressure.
    Nominal,
    /// The shard is showing scheduling pressure.
    Pressured,
    /// The controller observed pressure but suppressed scale-out or rollover.
    Suppressed,
}

/// Read-only runtime facts for one HighPriority runtime shard.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighPriorityRuntimeShardSnapshot {
    /// Runtime shard identity.
    pub shard_id: HighPriorityShardId,
    /// Tokio worker threads owned by this shard.
    pub worker_threads: usize,
    /// Wall-clock creation time.
    pub created_at: DateTime<Utc>,
    /// Currently running service generations assigned to this shard.
    pub active_generations: usize,
    /// Service instances last assigned to this shard.
    pub assigned_instances: usize,
    /// Best-effort pressure state derived from policy observations.
    pub pressure_state: HighPriorityShardPressureState,
}

/// Daemon-level runtime facts.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonRuntimeSnapshot {
    /// Stable identifier for this daemon resource instance.
    pub daemon_id: DaemonInstanceId,
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
    /// Framework-owned HighPriority runtime shards.
    pub high_priority_shards: Vec<HighPriorityRuntimeShardSnapshot>,
    /// Snapshot generation time.
    pub generated_at: DateTime<Utc>,
}

/// Runtime facts for one managed service.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRuntimeSnapshot {
    /// Runtime service instance identity.
    pub service_instance_id: ServiceInstanceId,
    pub service_name: &'static str,
    pub priority: u8,
    pub declared_scheduling: ServiceScheduling,
    /// HighPriority shard that last ran this service generation, if applicable.
    pub high_priority_shard_id: Option<HighPriorityShardId>,
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
    pub service_instance_id: ServiceInstanceId,
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
    pub service_instance_id: ServiceInstanceId,
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
    pub service_instance_id: ServiceInstanceId,
    pub service_name: &'static str,
    pub generation: u64,
    /// Pressure facts observed by the trigger runner.
    pub pressure: TriggerPressureSnapshot,
}
