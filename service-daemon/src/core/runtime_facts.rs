use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::models::{
    DaemonInstanceId, DaemonRuntimeSnapshot, ReadinessServiceError, ReadinessSnapshot,
    ServiceInstanceId, ServiceInstanceRecord, ServiceRuntimeSnapshot, ServiceScheduling,
    ServiceStatus, TriggerPressureSnapshot, TriggerRuntimeSnapshot,
};
#[cfg(feature = "high-priority")]
use crate::models::{HighPriorityRuntimeShardSnapshot, HighPriorityShardId};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use parking_lot::Mutex;
use tokio::sync::Semaphore;

#[derive(Clone, Copy)]
pub(crate) struct ServiceRuntimeMetadata {
    service_instance_id: ServiceInstanceId,
    service_name: &'static str,
    priority: u8,
    declared_scheduling: ServiceScheduling,
}

impl From<&ServiceInstanceRecord> for ServiceRuntimeMetadata {
    fn from(service: &ServiceInstanceRecord) -> Self {
        Self {
            service_instance_id: service.instance_id(),
            service_name: service.name(),
            priority: service.priority(),
            declared_scheduling: service.scheduling(),
        }
    }
}

#[derive(Debug, Default)]
struct ServiceRuntimeState {
    generation: u64,
    restart_count: u64,
    last_started_at: Option<DateTime<Utc>>,
    last_stopped_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    current_backoff: Option<Duration>,
    healthy_since: Option<DateTime<Utc>>,
    #[cfg(feature = "high-priority")]
    high_priority_shard_id: Option<HighPriorityShardId>,
}

struct ServiceRuntimeRecord {
    metadata: ServiceRuntimeMetadata,
    state: Mutex<ServiceRuntimeState>,
}

impl ServiceRuntimeRecord {
    fn new(metadata: ServiceRuntimeMetadata) -> Self {
        Self {
            metadata,
            state: Mutex::new(ServiceRuntimeState::default()),
        }
    }

    fn snapshot(&self, status: ServiceStatus) -> ServiceRuntimeSnapshot {
        let state = self.state.lock();
        ServiceRuntimeSnapshot {
            service_instance_id: self.metadata.service_instance_id,
            service_name: self.metadata.service_name,
            priority: self.metadata.priority,
            declared_scheduling: self.metadata.declared_scheduling,
            #[cfg(feature = "high-priority")]
            high_priority_shard_id: state.high_priority_shard_id,
            status,
            generation: state.generation,
            restart_count: state.restart_count,
            last_started_at: state.last_started_at,
            last_stopped_at: state.last_stopped_at,
            last_error: state.last_error.clone(),
            current_backoff: state.current_backoff,
            healthy_since: state.healthy_since,
        }
    }
}

#[derive(Debug, Default)]
struct TriggerRuntimeTimeline {
    last_event_at: Option<DateTime<Utc>>,
    last_success_at: Option<DateTime<Utc>>,
    last_error_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    lagged_count: Option<u64>,
    dropped_count: Option<u64>,
    closed_count: Option<u64>,
    backpressure_count: Option<u64>,
}

struct TriggerRuntimeRecord {
    service_instance_id: ServiceInstanceId,
    service_name: &'static str,
    generation: u64,
    semaphore: Arc<Semaphore>,
    current_limit: Arc<AtomicUsize>,
    dispatched_total: AtomicU64,
    completed_total: AtomicU64,
    failed_total: AtomicU64,
    retry_total: AtomicU64,
    timeline: Mutex<TriggerRuntimeTimeline>,
}

impl TriggerRuntimeRecord {
    fn new(
        service_instance_id: ServiceInstanceId,
        service_name: &'static str,
        generation: u64,
        semaphore: Arc<Semaphore>,
        current_limit: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            service_instance_id,
            service_name,
            generation,
            semaphore,
            current_limit,
            dispatched_total: AtomicU64::new(0),
            completed_total: AtomicU64::new(0),
            failed_total: AtomicU64::new(0),
            retry_total: AtomicU64::new(0),
            timeline: Mutex::new(TriggerRuntimeTimeline::default()),
        }
    }

    fn pressure_snapshot(&self) -> TriggerPressureSnapshot {
        let current_limit = self.current_limit.load(Ordering::Relaxed);
        let available_permits = self.semaphore.available_permits();
        let in_flight = current_limit.saturating_sub(available_permits);
        let timeline = self.timeline.lock();
        TriggerPressureSnapshot {
            service_instance_id: self.service_instance_id,
            service_name: self.service_name,
            generation: self.generation,
            in_flight,
            current_limit,
            available_permits,
            dispatched_total: self.dispatched_total.load(Ordering::Relaxed),
            completed_total: self.completed_total.load(Ordering::Relaxed),
            failed_total: self.failed_total.load(Ordering::Relaxed),
            retry_total: self.retry_total.load(Ordering::Relaxed),
            last_event_at: timeline.last_event_at,
            last_success_at: timeline.last_success_at,
            last_error_at: timeline.last_error_at,
            last_error: timeline.last_error.clone(),
            lagged_count: timeline.lagged_count,
            dropped_count: timeline.dropped_count,
            closed_count: timeline.closed_count,
            backpressure_count: timeline.backpressure_count,
        }
    }

    fn runtime_snapshot(&self) -> TriggerRuntimeSnapshot {
        let pressure = self.pressure_snapshot();
        TriggerRuntimeSnapshot {
            service_instance_id: self.service_instance_id,
            service_name: self.service_name,
            generation: self.generation,
            pressure,
        }
    }
}

#[derive(Clone)]
pub(crate) struct TriggerRuntimeFactsHandle {
    record: Arc<TriggerRuntimeRecord>,
}

impl TriggerRuntimeFactsHandle {
    pub(crate) fn record_dispatched(&self) {
        self.record.dispatched_total.fetch_add(1, Ordering::Relaxed);
        self.record.timeline.lock().last_event_at = Some(Utc::now());
    }

    pub(crate) fn record_completed(&self) {
        self.record.completed_total.fetch_add(1, Ordering::Relaxed);
        self.record.timeline.lock().last_success_at = Some(Utc::now());
    }

    pub(crate) fn record_failed(&self, message: impl Into<String>) {
        self.record.failed_total.fetch_add(1, Ordering::Relaxed);
        let mut timeline = self.record.timeline.lock();
        timeline.last_error_at = Some(Utc::now());
        timeline.last_error = Some(message.into());
    }

    pub(crate) fn record_retry(&self, message: impl Into<String>) {
        self.record.retry_total.fetch_add(1, Ordering::Relaxed);
        let mut timeline = self.record.timeline.lock();
        timeline.last_error_at = Some(Utc::now());
        timeline.last_error = Some(message.into());
    }
}

pub(crate) struct RuntimeFactsStore {
    daemon_id: DaemonInstanceId,
    start_time: DateTime<Utc>,
    start_instant: Instant,
    services: DashMap<ServiceInstanceId, Arc<ServiceRuntimeRecord>>,
    triggers: DashMap<ServiceInstanceId, Arc<TriggerRuntimeRecord>>,
    #[cfg(feature = "high-priority")]
    high_priority_shards: DashMap<HighPriorityShardId, HighPriorityRuntimeShardSnapshot>,
}

impl Default for RuntimeFactsStore {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeFactsStore {
    pub(crate) fn new() -> Self {
        Self::new_for_daemon(DaemonInstanceId::new_v7())
    }

    pub(crate) fn new_for_daemon(daemon_id: DaemonInstanceId) -> Self {
        Self {
            daemon_id,
            start_time: Utc::now(),
            start_instant: Instant::now(),
            services: DashMap::new(),
            triggers: DashMap::new(),
            #[cfg(feature = "high-priority")]
            high_priority_shards: DashMap::new(),
        }
    }

    pub(crate) fn daemon_id(&self) -> DaemonInstanceId {
        self.daemon_id
    }

    pub(crate) fn register_service_instances(&self, services: &[ServiceInstanceRecord]) {
        for service in services {
            let metadata = ServiceRuntimeMetadata::from(service);
            self.services
                .entry(service.instance_id())
                .or_insert_with(|| Arc::new(ServiceRuntimeRecord::new(metadata)));
        }
    }

    pub(crate) fn remove_service_instance(&self, service_instance_id: ServiceInstanceId) {
        self.services.remove(&service_instance_id);
        self.triggers.remove(&service_instance_id);
    }

    pub(crate) fn daemon_snapshot(&self, shutdown_requested: bool) -> DaemonRuntimeSnapshot {
        DaemonRuntimeSnapshot {
            daemon_id: self.daemon_id,
            start_time: self.start_time,
            uptime: self.start_instant.elapsed(),
            shutdown_requested,
            service_count: self.services.len(),
            trigger_count: self.triggers.len(),
            #[cfg(feature = "high-priority")]
            high_priority_shards: self.high_priority_shard_snapshots(),
            generated_at: Utc::now(),
        }
    }

    #[cfg(feature = "high-priority")]
    pub(crate) fn record_high_priority_shards(
        &self,
        shards: Vec<HighPriorityRuntimeShardSnapshot>,
    ) {
        self.high_priority_shards.clear();
        for shard in shards {
            self.high_priority_shards.insert(shard.shard_id, shard);
        }
    }

    #[cfg(feature = "high-priority")]
    fn high_priority_shard_snapshots(&self) -> Vec<HighPriorityRuntimeShardSnapshot> {
        let mut snapshots: Vec<_> = self
            .high_priority_shards
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        snapshots.sort_by_key(|snapshot| snapshot.shard_id);
        snapshots
    }

    #[cfg(feature = "high-priority")]
    pub(crate) fn record_service_high_priority_shard(
        &self,
        service_instance_id: ServiceInstanceId,
        shard_id: Option<HighPriorityShardId>,
    ) {
        let Some(record) = self.services.get(&service_instance_id) else {
            return;
        };
        let mut state = record.state.lock();
        state.high_priority_shard_id = shard_id;
    }

    pub(crate) fn record_service_started(
        &self,
        service_instance_id: ServiceInstanceId,
        generation: u64,
        status: &ServiceStatus,
    ) {
        let Some(record) = self.services.get(&service_instance_id) else {
            return;
        };
        let mut state = record.state.lock();
        state.generation = generation;
        state.last_started_at = Some(Utc::now());
        state.current_backoff = None;
        state.healthy_since = None;
        record_status_facts(&mut state, status);
    }

    pub(crate) fn record_service_status(
        &self,
        service_instance_id: ServiceInstanceId,
        status: &ServiceStatus,
    ) {
        let Some(record) = self.services.get(&service_instance_id) else {
            return;
        };
        let mut state = record.state.lock();
        record_status_facts(&mut state, status);
    }

    pub(crate) fn record_service_restart(
        &self,
        service_instance_id: ServiceInstanceId,
        backoff: Option<Duration>,
    ) {
        let Some(record) = self.services.get(&service_instance_id) else {
            return;
        };
        let mut state = record.state.lock();
        state.restart_count = state.restart_count.saturating_add(1);
        state.current_backoff = backoff;
    }

    pub(crate) fn service_snapshot<F>(
        &self,
        service_instance_id: ServiceInstanceId,
        status_for: F,
    ) -> Option<ServiceRuntimeSnapshot>
    where
        F: Fn(ServiceInstanceId) -> ServiceStatus,
    {
        self.services
            .get(&service_instance_id)
            .map(|record| record.snapshot(status_for(service_instance_id)))
    }

    pub(crate) fn service_snapshots<F>(&self, status_for: F) -> Vec<ServiceRuntimeSnapshot>
    where
        F: Fn(ServiceInstanceId) -> ServiceStatus,
    {
        let mut snapshots: Vec<_> = self
            .services
            .iter()
            .map(|record| {
                let service_instance_id = *record.key();
                record.value().snapshot(status_for(service_instance_id))
            })
            .collect();
        snapshots.sort_by_key(|snapshot| snapshot.service_instance_id);
        snapshots
    }

    pub(crate) fn readiness_snapshot<F>(&self, status_for: F) -> ReadinessSnapshot
    where
        F: Fn(ServiceInstanceId) -> ServiceStatus,
    {
        let services = self.service_snapshots(status_for);
        readiness_from_services(services)
    }

    pub(crate) fn register_trigger(
        &self,
        service_instance_id: ServiceInstanceId,
        service_name: &'static str,
        generation: u64,
        semaphore: Arc<Semaphore>,
        current_limit: Arc<AtomicUsize>,
    ) -> TriggerRuntimeFactsHandle {
        let record = Arc::new(TriggerRuntimeRecord::new(
            service_instance_id,
            service_name,
            generation,
            semaphore,
            current_limit,
        ));
        self.triggers.insert(service_instance_id, record.clone());
        TriggerRuntimeFactsHandle { record }
    }

    pub(crate) fn trigger_snapshot(
        &self,
        service_instance_id: ServiceInstanceId,
    ) -> Option<TriggerRuntimeSnapshot> {
        self.triggers
            .get(&service_instance_id)
            .map(|record| record.runtime_snapshot())
    }

    pub(crate) fn trigger_snapshots(&self) -> Vec<TriggerRuntimeSnapshot> {
        let mut snapshots: Vec<_> = self
            .triggers
            .iter()
            .map(|record| record.runtime_snapshot())
            .collect();
        snapshots.sort_by_key(|snapshot| snapshot.service_instance_id);
        snapshots
    }

    pub(crate) fn trigger_pressure(
        &self,
        service_instance_id: ServiceInstanceId,
    ) -> Option<TriggerPressureSnapshot> {
        self.triggers
            .get(&service_instance_id)
            .map(|record| record.pressure_snapshot())
    }
}

fn record_status_facts(state: &mut ServiceRuntimeState, status: &ServiceStatus) {
    match status {
        ServiceStatus::Healthy => {
            state.healthy_since.get_or_insert_with(Utc::now);
            state.current_backoff = None;
        }
        ServiceStatus::Recovering(message) => {
            state.healthy_since = None;
            state.last_error = Some(message.clone());
            state.last_stopped_at = Some(Utc::now());
        }
        ServiceStatus::Terminated => {
            state.healthy_since = None;
            state.last_stopped_at = Some(Utc::now());
            state.current_backoff = None;
        }
        ServiceStatus::Initializing | ServiceStatus::Restoring => {
            state.healthy_since = None;
            state.current_backoff = None;
        }
        ServiceStatus::NeedReload => {
            state.healthy_since = None;
        }
        ServiceStatus::ShuttingDown => {
            state.healthy_since = None;
            state.last_stopped_at.get_or_insert_with(Utc::now);
        }
    }
}

fn readiness_from_services(services: Vec<ServiceRuntimeSnapshot>) -> ReadinessSnapshot {
    let mut snapshot = ReadinessSnapshot {
        healthy: Vec::new(),
        initializing: Vec::new(),
        recovering: Vec::new(),
        restoring: Vec::new(),
        need_reload: Vec::new(),
        shutting_down: Vec::new(),
        terminated: Vec::new(),
        recent_errors: Vec::new(),
        generated_at: Utc::now(),
    };

    for service in services {
        if let ServiceStatus::Recovering(message) = &service.status {
            snapshot.recent_errors.push(ReadinessServiceError {
                service_instance_id: service.service_instance_id,
                service_name: service.service_name,
                status: service.status.clone(),
                message: message.clone(),
            });
        } else if let Some(message) = service.last_error.clone() {
            snapshot.recent_errors.push(ReadinessServiceError {
                service_instance_id: service.service_instance_id,
                service_name: service.service_name,
                status: service.status.clone(),
                message,
            });
        }

        match service.status {
            ServiceStatus::Healthy => snapshot.healthy.push(service),
            ServiceStatus::Initializing => snapshot.initializing.push(service),
            ServiceStatus::Recovering(_) => snapshot.recovering.push(service),
            ServiceStatus::Restoring => snapshot.restoring.push(service),
            ServiceStatus::NeedReload => snapshot.need_reload.push(service),
            ServiceStatus::ShuttingDown => snapshot.shutting_down.push(service),
            ServiceStatus::Terminated => snapshot.terminated.push(service),
        }
    }

    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        ServiceEntry, ServiceEntryId, ServiceInstanceRecord, ServiceInvocationContext, ServiceParam,
    };
    use futures::future::BoxFuture;
    use tokio_util::sync::CancellationToken;

    fn noop_service(_context: ServiceInvocationContext) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    static SERVICE_ENTRY_A: ServiceEntry = ServiceEntry {
        name: "alpha",
        module: "runtime_facts_tests",
        params: &[] as &[ServiceParam],
        wrapper: noop_service,
        watcher: None,
        priority: 50,
        scheduling: ServiceScheduling::Standard,
        input: None,
        tags: &[],
    };

    static SERVICE_ENTRY_B: ServiceEntry = ServiceEntry {
        name: "beta",
        module: "runtime_facts_tests",
        params: &[] as &[ServiceParam],
        wrapper: noop_service,
        watcher: None,
        priority: 80,
        scheduling: ServiceScheduling::Isolated,
        input: None,
        tags: &[],
    };

    fn service_instance_record(id: usize, entry: &'static ServiceEntry) -> ServiceInstanceRecord {
        let entry_id = ServiceEntryId::new(id);
        ServiceInstanceRecord::new(
            ServiceInstanceId::new(uuid::Uuid::from_u128(id as u128)),
            entry_id,
            entry,
            CancellationToken::new(),
        )
    }

    #[test]
    fn service_snapshots_are_sorted_by_service_id() {
        let store = RuntimeFactsStore::new();
        let services = vec![
            service_instance_record(2, &SERVICE_ENTRY_B),
            service_instance_record(1, &SERVICE_ENTRY_A),
        ];
        store.register_service_instances(&services);

        let snapshots = store.service_snapshots(|_| ServiceStatus::Healthy);

        assert_eq!(
            snapshots
                .iter()
                .map(|snapshot| snapshot.service_instance_id)
                .collect::<Vec<_>>(),
            vec![
                ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
                ServiceInstanceId::new(uuid::Uuid::from_u128(2))
            ]
        );
    }

    #[test]
    fn readiness_groups_statuses_and_errors_without_verdict() {
        let store = RuntimeFactsStore::new();
        let services = vec![
            service_instance_record(1, &SERVICE_ENTRY_A),
            service_instance_record(2, &SERVICE_ENTRY_B),
        ];
        store.register_service_instances(&services);
        store.record_service_status(
            ServiceInstanceId::new(uuid::Uuid::from_u128(2)),
            &ServiceStatus::Recovering("boom".into()),
        );

        let readiness = store.readiness_snapshot(|service_instance_id| {
            if service_instance_id == ServiceInstanceId::new(uuid::Uuid::from_u128(1)) {
                ServiceStatus::Healthy
            } else {
                ServiceStatus::Recovering("boom".into())
            }
        });

        assert_eq!(readiness.healthy.len(), 1);
        assert_eq!(readiness.recovering.len(), 1);
        assert_eq!(readiness.recent_errors.len(), 1);
        assert_eq!(readiness.recent_errors[0].message, "boom");
    }

    #[test]
    fn service_lifecycle_facts_record_generation_backoff_and_health() {
        let store = RuntimeFactsStore::new();
        let services = vec![service_instance_record(1, &SERVICE_ENTRY_A)];
        store.register_service_instances(&services);

        store.record_service_started(
            ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            3,
            &ServiceStatus::Initializing,
        );
        store.record_service_status(
            ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            &ServiceStatus::Healthy,
        );
        store.record_service_restart(
            ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            Some(Duration::from_millis(25)),
        );

        let snapshot = store
            .service_snapshot(ServiceInstanceId::new(uuid::Uuid::from_u128(1)), |_| {
                ServiceStatus::Healthy
            })
            .expect("registered service should have a snapshot");
        assert_eq!(snapshot.generation, 3);
        assert_eq!(snapshot.restart_count, 1);
        assert_eq!(snapshot.current_backoff, Some(Duration::from_millis(25)));
        assert!(snapshot.last_started_at.is_some());
        assert!(snapshot.healthy_since.is_some());
    }

    #[test]
    fn recovering_clears_health_and_records_error_boundary() {
        let store = RuntimeFactsStore::new();
        let services = vec![service_instance_record(1, &SERVICE_ENTRY_A)];
        store.register_service_instances(&services);

        store.record_service_started(
            ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            1,
            &ServiceStatus::Initializing,
        );
        store.record_service_status(
            ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            &ServiceStatus::Healthy,
        );
        store.record_service_status(
            ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            &ServiceStatus::Recovering("retryable failure".into()),
        );

        let snapshot = store
            .service_snapshot(ServiceInstanceId::new(uuid::Uuid::from_u128(1)), |_| {
                ServiceStatus::Recovering("retryable failure".into())
            })
            .expect("registered service should have a snapshot");
        assert_eq!(snapshot.last_error.as_deref(), Some("retryable failure"));
        assert!(snapshot.last_stopped_at.is_some());
        assert_eq!(snapshot.healthy_since, None);
    }

    #[test]
    fn shutdown_and_termination_clear_health_timeline() {
        let store = RuntimeFactsStore::new();
        let services = vec![service_instance_record(1, &SERVICE_ENTRY_A)];
        store.register_service_instances(&services);

        store.record_service_started(
            ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            1,
            &ServiceStatus::Initializing,
        );
        store.record_service_status(
            ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            &ServiceStatus::Healthy,
        );
        store.record_service_status(
            ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            &ServiceStatus::ShuttingDown,
        );

        let shutting_down = store
            .service_snapshot(ServiceInstanceId::new(uuid::Uuid::from_u128(1)), |_| {
                ServiceStatus::ShuttingDown
            })
            .expect("registered service should have a shutdown snapshot");
        assert_eq!(shutting_down.healthy_since, None);
        assert!(shutting_down.last_stopped_at.is_some());

        store.record_service_status(
            ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            &ServiceStatus::Healthy,
        );
        store.record_service_status(
            ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            &ServiceStatus::Terminated,
        );

        let terminated = store
            .service_snapshot(ServiceInstanceId::new(uuid::Uuid::from_u128(1)), |_| {
                ServiceStatus::Terminated
            })
            .expect("registered service should have a terminated snapshot");
        assert_eq!(terminated.healthy_since, None);
        assert!(terminated.last_stopped_at.is_some());
        assert_eq!(terminated.current_backoff, None);
    }

    #[test]
    fn restart_backoff_survives_until_terminal_or_running_boundary() {
        let store = RuntimeFactsStore::new();
        let services = vec![service_instance_record(1, &SERVICE_ENTRY_A)];
        store.register_service_instances(&services);

        store.record_service_started(
            ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            1,
            &ServiceStatus::Initializing,
        );
        store.record_service_restart(
            ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            Some(Duration::from_millis(50)),
        );
        store.record_service_status(
            ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            &ServiceStatus::Recovering("boom".into()),
        );

        let recovering = store
            .service_snapshot(ServiceInstanceId::new(uuid::Uuid::from_u128(1)), |_| {
                ServiceStatus::Recovering("boom".into())
            })
            .expect("registered service should have a recovering snapshot");
        assert_eq!(recovering.current_backoff, Some(Duration::from_millis(50)));

        store.record_service_status(
            ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            &ServiceStatus::Terminated,
        );
        let terminated = store
            .service_snapshot(ServiceInstanceId::new(uuid::Uuid::from_u128(1)), |_| {
                ServiceStatus::Terminated
            })
            .expect("registered service should have a terminated snapshot");
        assert_eq!(terminated.current_backoff, None);
    }
}
