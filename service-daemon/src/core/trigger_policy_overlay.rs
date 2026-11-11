use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use parking_lot::Mutex;
use tokio::sync::Semaphore;
use tracing::{info, warn};

use crate::models::policy::validate_overlay_clear_reason;
use crate::models::{
    RestartPolicy, ScalingPolicy, ServiceInstanceId, TriggerPolicyOverlay,
    TriggerPolicyOverlayError,
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct TriggerBasePolicy {
    pub(crate) restart_policy: RestartPolicy,
    pub(crate) scaling: Option<ScalingPolicy>,
}

impl TriggerBasePolicy {
    pub(crate) fn max_concurrency(self) -> usize {
        self.scaling.map_or(1, |policy| policy.max_concurrency())
    }

    pub(crate) fn initial_concurrency(self) -> usize {
        self.scaling
            .map_or(1, |policy| policy.initial_concurrency())
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct EffectiveTriggerPolicy {
    pub(crate) restart_policy: RestartPolicy,
    pub(crate) dispatch_timeout: Option<Duration>,
}

#[derive(Clone, Debug)]
struct ActiveTriggerPolicyOverlay {
    overlay: TriggerPolicyOverlay,
    expires_at: Instant,
}

#[derive(Debug, Default)]
struct TriggerPolicyOverlayState {
    overlay: Option<ActiveTriggerPolicyOverlay>,
    restore_base_on_next_reconcile: bool,
}

#[derive(Clone, Debug)]
struct ExpiredTriggerPolicyOverlay {
    overlay: TriggerPolicyOverlay,
    had_concurrency: bool,
}

#[derive(Debug)]
struct TriggerPolicyOverlayRecord {
    base: TriggerBasePolicy,
    generation: u64,
    semaphore: Arc<Semaphore>,
    current_limit: Arc<AtomicUsize>,
    state: Mutex<TriggerPolicyOverlayState>,
    reconcile: Mutex<()>,
}

impl TriggerPolicyOverlayRecord {
    fn new(
        base: TriggerBasePolicy,
        generation: u64,
        semaphore: Arc<Semaphore>,
        current_limit: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            base,
            generation,
            semaphore,
            current_limit,
            state: Mutex::new(TriggerPolicyOverlayState::default()),
            reconcile: Mutex::new(()),
        }
    }
}

#[derive(Default)]
pub(crate) struct TriggerPolicyOverlayStore {
    records: DashMap<ServiceInstanceId, Arc<TriggerPolicyOverlayRecord>>,
}

impl TriggerPolicyOverlayStore {
    pub(crate) fn register_trigger(
        &self,
        service_instance_id: ServiceInstanceId,
        generation: u64,
        base: TriggerBasePolicy,
        semaphore: Arc<Semaphore>,
        current_limit: Arc<AtomicUsize>,
    ) {
        self.records.insert(
            service_instance_id,
            Arc::new(TriggerPolicyOverlayRecord::new(
                base,
                generation,
                semaphore,
                current_limit,
            )),
        );
    }

    pub(crate) fn remove_trigger_generation(
        &self,
        service_instance_id: ServiceInstanceId,
        generation: u64,
    ) {
        let Some((_, record)) = self.records.remove_if(&service_instance_id, |_, record| {
            record.generation == generation
        }) else {
            return;
        };

        let mut state = record.state.lock();
        let cleared = state.overlay.take();
        let had_concurrency = cleared
            .as_ref()
            .and_then(|active| active.overlay.concurrency_limit())
            .is_some();
        drop(state);

        Self::emit_overlay_cleared(
            service_instance_id,
            generation,
            "generation_end",
            "trigger generation ended",
            cleared.as_ref().map(|active| &active.overlay),
            cleared.is_some(),
            had_concurrency,
        );
    }

    pub(crate) fn request_overlay(
        &self,
        service_instance_id: ServiceInstanceId,
        generation: u64,
        overlay: TriggerPolicyOverlay,
    ) -> Result<(), TriggerPolicyOverlayError> {
        if let Err(error) = overlay.validate_shape() {
            Self::emit_overlay_rejected(service_instance_id, generation, Some(&overlay), &error);
            return Err(error);
        }
        let Some(record) = self.record_for_generation(service_instance_id, generation) else {
            let error = TriggerPolicyOverlayError::TriggerOverlayUnavailable;
            Self::emit_overlay_rejected(service_instance_id, generation, Some(&overlay), &error);
            return Err(error);
        };
        if let Err(error) = self.validate_overlay_bounds(&record, &overlay) {
            Self::emit_overlay_rejected(service_instance_id, generation, Some(&overlay), &error);
            return Err(error);
        }
        let expires_at = Instant::now() + overlay.ttl();
        let mut state = record.state.lock();
        let previous_had_concurrency = state
            .overlay
            .as_ref()
            .and_then(|active| active.overlay.concurrency_limit())
            .is_some();
        let next_has_concurrency = overlay.concurrency_limit().is_some();
        state.overlay = Some(ActiveTriggerPolicyOverlay {
            overlay: overlay.clone(),
            expires_at,
        });
        state.restore_base_on_next_reconcile = previous_had_concurrency && !next_has_concurrency;
        Self::emit_overlay_accepted(service_instance_id, generation, &overlay);
        Ok(())
    }

    pub(crate) fn clear_overlay(
        &self,
        service_instance_id: ServiceInstanceId,
        generation: u64,
        reason: &str,
    ) -> Result<(), TriggerPolicyOverlayError> {
        if let Err(error) = validate_overlay_clear_reason(reason) {
            Self::emit_overlay_rejected(service_instance_id, generation, None, &error);
            return Err(error);
        }
        let Some(record) = self.record_for_generation(service_instance_id, generation) else {
            let error = TriggerPolicyOverlayError::TriggerOverlayUnavailable;
            Self::emit_overlay_rejected(service_instance_id, generation, None, &error);
            return Err(error);
        };
        let mut state = record.state.lock();
        let cleared = state.overlay.take();
        let had_overlay = cleared.is_some();
        let had_concurrency = cleared
            .as_ref()
            .and_then(|active| active.overlay.concurrency_limit())
            .is_some();
        if had_concurrency {
            state.restore_base_on_next_reconcile = true;
        }
        drop(state);
        Self::emit_overlay_cleared(
            service_instance_id,
            generation,
            "manual",
            reason,
            cleared.as_ref().map(|active| &active.overlay),
            had_overlay,
            had_concurrency,
        );
        Ok(())
    }

    pub(crate) fn effective_policy(
        &self,
        service_instance_id: ServiceInstanceId,
        generation: u64,
        fallback: TriggerBasePolicy,
    ) -> EffectiveTriggerPolicy {
        let Some(record) = self.record_for_generation(service_instance_id, generation) else {
            return EffectiveTriggerPolicy {
                restart_policy: fallback.restart_policy,
                dispatch_timeout: None,
            };
        };
        self.prune_expired_overlay(&record, service_instance_id);
        let state = record.state.lock();
        let Some(active) = state.overlay.as_ref() else {
            return EffectiveTriggerPolicy {
                restart_policy: record.base.restart_policy,
                dispatch_timeout: None,
            };
        };
        EffectiveTriggerPolicy {
            restart_policy: active
                .overlay
                .retry_policy()
                .unwrap_or(record.base.restart_policy),
            dispatch_timeout: active.overlay.dispatch_timeout(),
        }
    }

    pub(crate) fn apply_effective_concurrency(
        &self,
        service_instance_id: ServiceInstanceId,
        generation: u64,
    ) {
        self.reconcile_effective_concurrency(
            service_instance_id,
            generation,
            self.current_limit_or_base(service_instance_id, generation),
        );
    }

    pub(crate) fn reconcile_effective_concurrency(
        &self,
        service_instance_id: ServiceInstanceId,
        generation: u64,
        base_target: usize,
    ) -> usize {
        let Some(record) = self.record_for_generation(service_instance_id, generation) else {
            return base_target.max(1);
        };
        let _reconcile_guard = record.reconcile.lock();
        let (expired, desired) = {
            let mut state = record.state.lock();
            let expired = Self::take_expired_overlay(&mut state);
            let desired = if let Some(limit) = state
                .overlay
                .as_ref()
                .and_then(|active| active.overlay.concurrency_limit())
            {
                state.restore_base_on_next_reconcile = false;
                limit
            } else if state.restore_base_on_next_reconcile {
                state.restore_base_on_next_reconcile = false;
                record.base.initial_concurrency()
            } else {
                base_target
            };
            (expired, desired)
        };
        if let Some(expired) = expired {
            Self::emit_overlay_expired(
                service_instance_id,
                record.generation,
                &expired.overlay,
                expired.had_concurrency,
            );
        }
        Self::reconcile_concurrency_limit(&record, desired)
    }

    pub(crate) fn effective_concurrency_limit(
        &self,
        service_instance_id: ServiceInstanceId,
        generation: u64,
        fallback: usize,
    ) -> usize {
        let Some(record) = self.record_for_generation(service_instance_id, generation) else {
            return fallback;
        };
        self.prune_expired_overlay(&record, service_instance_id);
        self.active_concurrency_limit_for(&record)
            .unwrap_or(fallback)
    }

    #[cfg(test)]
    pub(crate) fn has_active_overlay(
        &self,
        service_instance_id: ServiceInstanceId,
        generation: u64,
    ) -> bool {
        self.record_for_generation(service_instance_id, generation)
            .is_some_and(|record| record.state.lock().overlay.is_some())
    }

    fn validate_overlay_bounds(
        &self,
        record: &TriggerPolicyOverlayRecord,
        overlay: &TriggerPolicyOverlay,
    ) -> Result<(), TriggerPolicyOverlayError> {
        if let Some(limit) = overlay.concurrency_limit() {
            let max = record.base.max_concurrency();
            if limit > max {
                return Err(TriggerPolicyOverlayError::ConcurrencyLimitExceedsMax {
                    requested: limit,
                    max,
                });
            }
        }
        Ok(())
    }

    fn record_for_generation(
        &self,
        service_instance_id: ServiceInstanceId,
        generation: u64,
    ) -> Option<Arc<TriggerPolicyOverlayRecord>> {
        self.records.get(&service_instance_id).and_then(|record| {
            if record.generation == generation {
                Some(record.value().clone())
            } else {
                None
            }
        })
    }

    fn prune_expired_overlay(
        &self,
        record: &TriggerPolicyOverlayRecord,
        service_instance_id: ServiceInstanceId,
    ) {
        let expired = {
            let mut state = record.state.lock();
            Self::take_expired_overlay(&mut state)
        };
        if let Some(expired) = expired {
            Self::emit_overlay_expired(
                service_instance_id,
                record.generation,
                &expired.overlay,
                expired.had_concurrency,
            );
        }
    }

    fn active_concurrency_limit_for(&self, record: &TriggerPolicyOverlayRecord) -> Option<usize> {
        record
            .state
            .lock()
            .overlay
            .as_ref()
            .and_then(|active| active.overlay.concurrency_limit())
    }

    fn current_limit_or_base(
        &self,
        service_instance_id: ServiceInstanceId,
        generation: u64,
    ) -> usize {
        self.record_for_generation(service_instance_id, generation)
            .map(|record| record.current_limit.load(Ordering::Relaxed))
            .unwrap_or(1)
    }

    fn take_expired_overlay(
        state: &mut TriggerPolicyOverlayState,
    ) -> Option<ExpiredTriggerPolicyOverlay> {
        let expired = state
            .overlay
            .as_ref()
            .is_some_and(|active| Instant::now() >= active.expires_at);
        if !expired {
            return None;
        }
        let active = state.overlay.take()?;
        let had_concurrency = active.overlay.concurrency_limit().is_some();
        if had_concurrency {
            state.restore_base_on_next_reconcile = true;
        }
        Some(ExpiredTriggerPolicyOverlay {
            overlay: active.overlay,
            had_concurrency,
        })
    }

    fn reconcile_concurrency_limit(record: &TriggerPolicyOverlayRecord, desired: usize) -> usize {
        let desired = desired.min(record.base.max_concurrency()).max(1);
        let current = record.current_limit.load(Ordering::Relaxed);
        let available = record.semaphore.available_permits();
        if desired > current {
            record.semaphore.add_permits(desired - current);
            record.current_limit.store(desired, Ordering::Relaxed);
            desired
        } else if desired < current {
            let in_flight = current.saturating_sub(available);
            let target = desired.max(in_flight);
            let mut revoked = 0usize;
            for _ in 0..current.saturating_sub(target) {
                match record.semaphore.try_acquire() {
                    Ok(permit) => {
                        permit.forget();
                        revoked += 1;
                    }
                    Err(error) => {
                        warn!(
                            generation = record.generation,
                            error = %error,
                            "Could not revoke trigger policy overlay permit"
                        );
                        break;
                    }
                }
            }
            record
                .current_limit
                .store(current.saturating_sub(revoked), Ordering::Relaxed);
            if revoked > 0 {
                info!(
                    generation = record.generation,
                    desired,
                    current_limit = current.saturating_sub(revoked),
                    revoked,
                    "Trigger policy overlay revoked idle permits"
                );
            }
            current.saturating_sub(revoked)
        } else {
            current
        }
    }

    fn emit_overlay_accepted(
        service_instance_id: ServiceInstanceId,
        generation: u64,
        overlay: &TriggerPolicyOverlay,
    ) {
        info!(
            event_kind = "trigger_policy_overlay_accepted",
            service_instance_id = %service_instance_id,
            generation,
            reason = overlay.reason(),
            ttl_ms = Self::duration_ms(overlay.ttl()),
            has_concurrency_limit = overlay.concurrency_limit().is_some(),
            concurrency_limit = overlay.concurrency_limit().unwrap_or(0),
            has_dispatch_timeout = overlay.dispatch_timeout().is_some(),
            dispatch_timeout_ms =
                Self::optional_duration_ms(overlay.dispatch_timeout()).unwrap_or(0),
            has_retry_policy = overlay.retry_policy().is_some(),
            "Trigger policy overlay accepted"
        );
    }

    fn emit_overlay_rejected(
        service_instance_id: ServiceInstanceId,
        generation: u64,
        overlay: Option<&TriggerPolicyOverlay>,
        error: &TriggerPolicyOverlayError,
    ) {
        warn!(
            event_kind = "trigger_policy_overlay_rejected",
            service_instance_id = %service_instance_id,
            generation,
            error_kind = error.kind(),
            error = %error,
            reason = overlay.map(TriggerPolicyOverlay::reason).unwrap_or(""),
            ttl_ms = overlay.map(|overlay| Self::duration_ms(overlay.ttl())).unwrap_or(0),
            has_concurrency_limit = overlay
                .and_then(TriggerPolicyOverlay::concurrency_limit)
                .is_some(),
            concurrency_limit = overlay
                .and_then(TriggerPolicyOverlay::concurrency_limit)
                .unwrap_or(0),
            has_dispatch_timeout = overlay
                .and_then(TriggerPolicyOverlay::dispatch_timeout)
                .is_some(),
            dispatch_timeout_ms = overlay
                .and_then(|overlay| Self::optional_duration_ms(overlay.dispatch_timeout()))
                .unwrap_or(0),
            has_retry_policy = overlay
                .and_then(TriggerPolicyOverlay::retry_policy)
                .is_some(),
            requested_concurrency_limit = Self::rejected_requested_concurrency(error).unwrap_or(0),
            max_concurrency_limit = Self::rejected_max_concurrency(error).unwrap_or(0),
            "Trigger policy overlay rejected"
        );
    }

    fn emit_overlay_cleared(
        service_instance_id: ServiceInstanceId,
        generation: u64,
        clear_source: &'static str,
        clear_reason: &str,
        overlay: Option<&TriggerPolicyOverlay>,
        had_overlay: bool,
        restored_base_policy: bool,
    ) {
        info!(
            event_kind = "trigger_policy_overlay_cleared",
            service_instance_id = %service_instance_id,
            generation,
            clear_source,
            clear_reason,
            had_overlay,
            restored_base_policy,
            overlay_reason = overlay.map(TriggerPolicyOverlay::reason).unwrap_or(""),
            ttl_ms = overlay
                .map(|overlay| Self::duration_ms(overlay.ttl()))
                .unwrap_or(0),
            has_concurrency_limit = overlay
                .and_then(TriggerPolicyOverlay::concurrency_limit)
                .is_some(),
            concurrency_limit = overlay
                .and_then(TriggerPolicyOverlay::concurrency_limit)
                .unwrap_or(0),
            has_dispatch_timeout = overlay
                .and_then(TriggerPolicyOverlay::dispatch_timeout)
                .is_some(),
            dispatch_timeout_ms = overlay
                .and_then(|overlay| Self::optional_duration_ms(overlay.dispatch_timeout()))
                .unwrap_or(0),
            has_retry_policy = overlay
                .and_then(TriggerPolicyOverlay::retry_policy)
                .is_some(),
            "Trigger policy overlay cleared"
        );
    }

    fn emit_overlay_expired(
        service_instance_id: ServiceInstanceId,
        generation: u64,
        overlay: &TriggerPolicyOverlay,
        restored_base_policy: bool,
    ) {
        info!(
            event_kind = "trigger_policy_overlay_expired",
            service_instance_id = %service_instance_id,
            generation,
            clear_source = "ttl_expired",
            reason = overlay.reason(),
            ttl_ms = Self::duration_ms(overlay.ttl()),
            restored_base_policy,
            has_concurrency_limit = overlay.concurrency_limit().is_some(),
            concurrency_limit = overlay.concurrency_limit().unwrap_or(0),
            has_dispatch_timeout = overlay.dispatch_timeout().is_some(),
            dispatch_timeout_ms =
                Self::optional_duration_ms(overlay.dispatch_timeout()).unwrap_or(0),
            has_retry_policy = overlay.retry_policy().is_some(),
            "Trigger policy overlay expired"
        );
    }

    fn duration_ms(duration: Duration) -> u64 {
        duration.as_millis().min(u128::from(u64::MAX)) as u64
    }

    fn optional_duration_ms(duration: Option<Duration>) -> Option<u64> {
        duration.map(Self::duration_ms)
    }

    fn rejected_requested_concurrency(error: &TriggerPolicyOverlayError) -> Option<usize> {
        match error {
            TriggerPolicyOverlayError::ConcurrencyLimitExceedsMax { requested, .. } => {
                Some(*requested)
            }
            _ => None,
        }
    }

    fn rejected_max_concurrency(error: &TriggerPolicyOverlayError) -> Option<usize> {
        match error {
            TriggerPolicyOverlayError::ConcurrencyLimitExceedsMax { max, .. } => Some(*max),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::TriggerPolicyOverlay;
    use std::collections::BTreeMap;
    use std::fmt;
    use std::sync::{LazyLock, Mutex as StdMutex};
    use tracing::field::{Field, Visit};
    use tracing::{Event, Subscriber};
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::prelude::*;

    static TRACE_CAPTURE_LOCK: LazyLock<StdMutex<()>> = LazyLock::new(|| StdMutex::new(()));

    #[derive(Clone, Default)]
    struct CapturedTraceFields {
        events: Arc<StdMutex<Vec<BTreeMap<String, String>>>>,
    }

    #[derive(Default)]
    struct TraceFieldVisitor {
        fields: BTreeMap<String, String>,
    }

    impl Visit for TraceFieldVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            self.fields
                .insert(field.name().to_string(), format!("{:?}", value));
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }

        fn record_bool(&mut self, field: &Field, value: bool) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }

        fn record_i64(&mut self, field: &Field, value: i64) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }

        fn record_u64(&mut self, field: &Field, value: u64) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }
    }

    impl<S> Layer<S> for CapturedTraceFields
    where
        S: Subscriber,
    {
        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            let mut visitor = TraceFieldVisitor::default();
            event.record(&mut visitor);
            self.events
                .lock()
                .unwrap_or_else(|err| panic!("trace capture lock poisoned: {err}"))
                .push(visitor.fields);
        }
    }

    fn with_trace_capture<F>(f: F) -> Vec<BTreeMap<String, String>>
    where
        F: FnOnce(),
    {
        let _guard = TRACE_CAPTURE_LOCK
            .lock()
            .unwrap_or_else(|err| panic!("trace capture test lock poisoned: {err}"));
        let capture = CapturedTraceFields::default();
        let events = capture.events.clone();
        let subscriber = tracing_subscriber::registry().with(capture);
        let _subscriber_guard = tracing::subscriber::set_default(subscriber);
        tracing::callsite::rebuild_interest_cache();
        f();
        let captured = events
            .lock()
            .unwrap_or_else(|err| panic!("trace capture lock poisoned: {err}"))
            .clone();
        drop(_subscriber_guard);
        tracing::callsite::rebuild_interest_cache();
        captured
    }

    fn overlay_event<'a>(
        events: &'a [BTreeMap<String, String>],
        event_kind: &str,
        service_instance_id: ServiceInstanceId,
    ) -> &'a BTreeMap<String, String> {
        let expected_service_id = service_instance_id.to_string();
        events
            .iter()
            .find(|event| {
                event
                    .get("event_kind")
                    .is_some_and(|kind| kind == event_kind)
                    && event
                        .get("service_instance_id")
                        .is_some_and(|actual| actual == &expected_service_id)
            })
            .unwrap_or_else(|| panic!("trace event {event_kind} should be captured: {events:?}"))
    }

    fn registered_store(
        service_instance_id: ServiceInstanceId,
        generation: u64,
        base: TriggerBasePolicy,
        current_limit: usize,
    ) -> (TriggerPolicyOverlayStore, Arc<Semaphore>, Arc<AtomicUsize>) {
        let store = TriggerPolicyOverlayStore::default();
        let semaphore = Arc::new(Semaphore::new(current_limit));
        let current_limit = Arc::new(AtomicUsize::new(current_limit));
        store.register_trigger(
            service_instance_id,
            generation,
            base,
            semaphore.clone(),
            current_limit.clone(),
        );
        (store, semaphore, current_limit)
    }

    #[test]
    fn overlay_rejects_concurrency_above_base_max() {
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(7));
        let base = TriggerBasePolicy {
            restart_policy: RestartPolicy::for_testing(),
            scaling: Some(ScalingPolicy::builder().max_concurrency(2).build()),
        };
        let (store, _, _) = registered_store(service_instance_id, 1, base, 1);

        let overlay = TriggerPolicyOverlay::builder("burst", Duration::from_secs(1))
            .concurrency_limit(3)
            .build()
            .expect("overlay shape should be valid");

        assert_eq!(
            store.request_overlay(service_instance_id, 1, overlay),
            Err(TriggerPolicyOverlayError::ConcurrencyLimitExceedsMax {
                requested: 3,
                max: 2
            })
        );
    }

    #[test]
    fn accepted_overlay_audit_includes_policy_fields() {
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(701));
        let base = TriggerBasePolicy {
            restart_policy: RestartPolicy::for_testing(),
            scaling: Some(ScalingPolicy::builder().max_concurrency(4).build()),
        };
        let (store, _, _) = registered_store(service_instance_id, 9, base, 1);
        let overlay = TriggerPolicyOverlay::builder("pressure relief", Duration::from_secs(3))
            .concurrency_limit(2)
            .dispatch_timeout(Duration::from_millis(250))
            .retry_policy(RestartPolicy::for_testing())
            .build()
            .expect("overlay shape should be valid");

        let events = with_trace_capture(|| {
            store
                .request_overlay(service_instance_id, 9, overlay)
                .expect("overlay should be accepted");
        });

        let event = overlay_event(
            &events,
            "trigger_policy_overlay_accepted",
            service_instance_id,
        );
        let expected_service_instance_id = service_instance_id.to_string();
        assert_eq!(
            event.get("service_instance_id").map(String::as_str),
            Some(expected_service_instance_id.as_str())
        );
        assert_eq!(event.get("generation").map(String::as_str), Some("9"));
        assert_eq!(
            event.get("reason").map(String::as_str),
            Some("pressure relief")
        );
        assert_eq!(event.get("ttl_ms").map(String::as_str), Some("3000"));
        assert_eq!(
            event.get("has_concurrency_limit").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            event.get("concurrency_limit").map(String::as_str),
            Some("2")
        );
        assert_eq!(
            event.get("has_dispatch_timeout").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            event.get("dispatch_timeout_ms").map(String::as_str),
            Some("250")
        );
        assert_eq!(
            event.get("has_retry_policy").map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn rejected_overlay_audit_includes_context_unavailable_error_kind() {
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(702));
        let overlay = TriggerPolicyOverlay::builder("not registered", Duration::from_secs(1))
            .concurrency_limit(1)
            .build()
            .expect("overlay shape should be valid");

        let events = with_trace_capture(|| {
            assert_eq!(
                TriggerPolicyOverlayStore::default().request_overlay(
                    service_instance_id,
                    1,
                    overlay
                ),
                Err(TriggerPolicyOverlayError::TriggerOverlayUnavailable)
            );
        });

        let event = overlay_event(
            &events,
            "trigger_policy_overlay_rejected",
            service_instance_id,
        );
        assert_eq!(
            event.get("error_kind").map(String::as_str),
            Some("trigger_overlay_unavailable")
        );
        assert_eq!(
            event.get("has_concurrency_limit").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            event.get("concurrency_limit").map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn rejected_overlay_audit_includes_bounds_fields() {
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(703));
        let base = TriggerBasePolicy {
            restart_policy: RestartPolicy::for_testing(),
            scaling: Some(ScalingPolicy::builder().max_concurrency(2).build()),
        };
        let (store, _, _) = registered_store(service_instance_id, 1, base, 1);
        let overlay = TriggerPolicyOverlay::builder("too high", Duration::from_secs(1))
            .concurrency_limit(3)
            .build()
            .expect("overlay shape should be valid");

        let events = with_trace_capture(|| {
            assert_eq!(
                store.request_overlay(service_instance_id, 1, overlay),
                Err(TriggerPolicyOverlayError::ConcurrencyLimitExceedsMax {
                    requested: 3,
                    max: 2
                })
            );
        });

        let event = overlay_event(
            &events,
            "trigger_policy_overlay_rejected",
            service_instance_id,
        );
        assert_eq!(
            event.get("error_kind").map(String::as_str),
            Some("concurrency_limit_exceeds_max")
        );
        assert_eq!(
            event.get("requested_concurrency_limit").map(String::as_str),
            Some("3")
        );
        assert_eq!(
            event.get("max_concurrency_limit").map(String::as_str),
            Some("2")
        );
    }

    #[test]
    fn clear_overlay_audit_distinguishes_present_and_empty_manual_clear() {
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(704));
        let base = TriggerBasePolicy {
            restart_policy: RestartPolicy::for_testing(),
            scaling: Some(
                ScalingPolicy::builder()
                    .initial_concurrency(1)
                    .max_concurrency(3)
                    .build(),
            ),
        };
        let (store, _, current_limit) = registered_store(service_instance_id, 1, base, 1);
        let overlay = TriggerPolicyOverlay::builder("manual test", Duration::from_secs(1))
            .concurrency_limit(3)
            .build()
            .expect("overlay shape should be valid");
        store
            .request_overlay(service_instance_id, 1, overlay)
            .expect("overlay should be accepted");
        assert_eq!(current_limit.load(Ordering::Relaxed), 1);

        let events = with_trace_capture(|| {
            store
                .clear_overlay(service_instance_id, 1, "manual recovery")
                .expect("manual clear should succeed");
            store
                .clear_overlay(service_instance_id, 1, "manual no-op")
                .expect("manual clear without active overlay should succeed");
        });

        assert_eq!(current_limit.load(Ordering::Relaxed), 1);
        let mut clear_events = events.iter().filter(|event| {
            event
                .get("event_kind")
                .is_some_and(|kind| kind == "trigger_policy_overlay_cleared")
        });
        let first = clear_events
            .next()
            .expect("first manual clear event should be captured");
        assert_eq!(
            first.get("clear_source").map(String::as_str),
            Some("manual")
        );
        assert_eq!(first.get("had_overlay").map(String::as_str), Some("true"));
        assert_eq!(
            first.get("restored_base_policy").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            first.get("overlay_reason").map(String::as_str),
            Some("manual test")
        );
        let second = clear_events
            .next()
            .expect("second manual clear event should be captured");
        assert_eq!(
            second.get("clear_source").map(String::as_str),
            Some("manual")
        );
        assert_eq!(second.get("had_overlay").map(String::as_str), Some("false"));
        assert_eq!(
            second.get("restored_base_policy").map(String::as_str),
            Some("false")
        );
    }

    #[tokio::test]
    async fn overlay_expires_back_to_initial_concurrency() {
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(8));
        let base = TriggerBasePolicy {
            restart_policy: RestartPolicy::for_testing(),
            scaling: Some(
                ScalingPolicy::builder()
                    .initial_concurrency(1)
                    .max_concurrency(4)
                    .build(),
            ),
        };
        let (store, _, _) = registered_store(service_instance_id, 1, base, 1);

        let overlay = TriggerPolicyOverlay::builder("burst", Duration::from_millis(10))
            .concurrency_limit(3)
            .build()
            .expect("overlay shape should be valid");
        store
            .request_overlay(service_instance_id, 1, overlay)
            .expect("overlay should be accepted");
        assert_eq!(
            store.effective_concurrency_limit(service_instance_id, 1, 1),
            3
        );

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            store.effective_concurrency_limit(service_instance_id, 1, 1),
            1
        );
    }

    #[test]
    fn ttl_expiry_audit_records_restore() {
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(705));
        let base = TriggerBasePolicy {
            restart_policy: RestartPolicy::for_testing(),
            scaling: Some(
                ScalingPolicy::builder()
                    .initial_concurrency(1)
                    .max_concurrency(4)
                    .build(),
            ),
        };
        let (store, _, current_limit) = registered_store(service_instance_id, 1, base, 1);
        let overlay = TriggerPolicyOverlay::builder("ttl test", Duration::from_millis(10))
            .concurrency_limit(3)
            .build()
            .expect("overlay shape should be valid");
        store
            .request_overlay(service_instance_id, 1, overlay)
            .expect("overlay should be accepted");

        let record = store
            .record_for_generation(service_instance_id, 1)
            .expect("overlay record should be registered");
        record
            .state
            .lock()
            .overlay
            .as_mut()
            .expect("overlay should be active")
            .expires_at = Instant::now() - Duration::from_millis(1);

        let events = with_trace_capture(|| {
            assert_eq!(
                store.effective_concurrency_limit(service_instance_id, 1, 1),
                1
            );
        });

        assert_eq!(current_limit.load(Ordering::Relaxed), 1);
        let event = overlay_event(
            &events,
            "trigger_policy_overlay_expired",
            service_instance_id,
        );
        assert_eq!(
            event.get("clear_source").map(String::as_str),
            Some("ttl_expired")
        );
        assert_eq!(event.get("reason").map(String::as_str), Some("ttl test"));
        assert_eq!(
            event.get("restored_base_policy").map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn generation_cleanup_audit_records_restore_and_removal() {
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(706));
        let base = TriggerBasePolicy {
            restart_policy: RestartPolicy::for_testing(),
            scaling: Some(
                ScalingPolicy::builder()
                    .initial_concurrency(1)
                    .max_concurrency(4)
                    .build(),
            ),
        };
        let (store, _, current_limit) = registered_store(service_instance_id, 12, base, 1);
        let overlay = TriggerPolicyOverlay::builder("generation end", Duration::from_secs(1))
            .concurrency_limit(3)
            .build()
            .expect("overlay shape should be valid");
        store
            .request_overlay(service_instance_id, 12, overlay)
            .expect("overlay should be accepted");
        assert_eq!(current_limit.load(Ordering::Relaxed), 1);

        let events = with_trace_capture(|| {
            store.remove_trigger_generation(service_instance_id, 12);
        });

        assert_eq!(current_limit.load(Ordering::Relaxed), 1);
        assert_eq!(
            store.effective_concurrency_limit(service_instance_id, 12, 4),
            4
        );
        let event = overlay_event(
            &events,
            "trigger_policy_overlay_cleared",
            service_instance_id,
        );
        assert_eq!(
            event.get("clear_source").map(String::as_str),
            Some("generation_end")
        );
        assert_eq!(event.get("had_overlay").map(String::as_str), Some("true"));
        assert_eq!(
            event.get("restored_base_policy").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            event.get("overlay_reason").map(String::as_str),
            Some("generation end")
        );
    }

    #[test]
    fn no_active_concurrency_overlay_preserves_scaling_fallback() {
        let store = TriggerPolicyOverlayStore::default();
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(10));
        let base = TriggerBasePolicy {
            restart_policy: RestartPolicy::for_testing(),
            scaling: Some(
                ScalingPolicy::builder()
                    .initial_concurrency(1)
                    .max_concurrency(4)
                    .build(),
            ),
        };
        let semaphore = Arc::new(Semaphore::new(2));
        let current_limit = Arc::new(AtomicUsize::new(2));
        store.register_trigger(
            service_instance_id,
            1,
            base,
            semaphore,
            current_limit.clone(),
        );

        assert_eq!(
            store.effective_concurrency_limit(service_instance_id, 1, 4),
            4
        );

        store.apply_effective_concurrency(service_instance_id, 1);
        assert_eq!(current_limit.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn non_concurrency_overlay_does_not_reset_scaled_limit() {
        let store = TriggerPolicyOverlayStore::default();
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(11));
        let base = TriggerBasePolicy {
            restart_policy: RestartPolicy::for_testing(),
            scaling: Some(
                ScalingPolicy::builder()
                    .initial_concurrency(1)
                    .max_concurrency(4)
                    .build(),
            ),
        };
        let semaphore = Arc::new(Semaphore::new(2));
        let current_limit = Arc::new(AtomicUsize::new(2));
        store.register_trigger(
            service_instance_id,
            1,
            base,
            semaphore,
            current_limit.clone(),
        );

        let overlay = TriggerPolicyOverlay::builder("retry tune", Duration::from_secs(1))
            .retry_policy(RestartPolicy::for_testing())
            .build()
            .expect("overlay shape should be valid");
        store
            .request_overlay(service_instance_id, 1, overlay)
            .expect("overlay should be accepted");

        assert_eq!(
            store.effective_concurrency_limit(service_instance_id, 1, 4),
            4
        );
        assert_eq!(current_limit.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn request_overlay_records_desired_state_until_reconcile() {
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(707));
        let base = TriggerBasePolicy {
            restart_policy: RestartPolicy::for_testing(),
            scaling: Some(
                ScalingPolicy::builder()
                    .initial_concurrency(1)
                    .max_concurrency(4)
                    .build(),
            ),
        };
        let (store, semaphore, current_limit) = registered_store(service_instance_id, 1, base, 1);
        let overlay = TriggerPolicyOverlay::builder("desired only", Duration::from_secs(1))
            .concurrency_limit(3)
            .build()
            .expect("overlay shape should be valid");

        store
            .request_overlay(service_instance_id, 1, overlay)
            .expect("overlay should be accepted");

        assert_eq!(current_limit.load(Ordering::Relaxed), 1);
        assert_eq!(semaphore.available_permits(), 1);
        assert_eq!(
            store.effective_concurrency_limit(service_instance_id, 1, 4),
            3
        );

        assert_eq!(
            store.reconcile_effective_concurrency(service_instance_id, 1, 1),
            3
        );
        assert_eq!(current_limit.load(Ordering::Relaxed), 3);
        assert_eq!(semaphore.available_permits(), 3);
    }

    #[test]
    fn clear_overlay_restores_base_on_next_reconcile() {
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(708));
        let base = TriggerBasePolicy {
            restart_policy: RestartPolicy::for_testing(),
            scaling: Some(
                ScalingPolicy::builder()
                    .initial_concurrency(1)
                    .max_concurrency(4)
                    .build(),
            ),
        };
        let (store, semaphore, current_limit) = registered_store(service_instance_id, 1, base, 1);
        let overlay = TriggerPolicyOverlay::builder("clear desired", Duration::from_secs(1))
            .concurrency_limit(3)
            .build()
            .expect("overlay shape should be valid");

        store
            .request_overlay(service_instance_id, 1, overlay)
            .expect("overlay should be accepted");
        assert_eq!(
            store.reconcile_effective_concurrency(service_instance_id, 1, 1),
            3
        );
        store
            .clear_overlay(service_instance_id, 1, "clear test")
            .expect("clear should be accepted");

        assert_eq!(current_limit.load(Ordering::Relaxed), 3);
        assert_eq!(semaphore.available_permits(), 3);
        assert_eq!(
            store.reconcile_effective_concurrency(service_instance_id, 1, 3),
            1
        );
        assert_eq!(current_limit.load(Ordering::Relaxed), 1);
        assert_eq!(semaphore.available_permits(), 1);
    }

    #[test]
    fn reconcile_does_not_revoke_in_flight_dispatches_below_target() {
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(709));
        let base = TriggerBasePolicy {
            restart_policy: RestartPolicy::for_testing(),
            scaling: Some(
                ScalingPolicy::builder()
                    .initial_concurrency(1)
                    .max_concurrency(4)
                    .build(),
            ),
        };
        let store = TriggerPolicyOverlayStore::default();
        let semaphore = Arc::new(Semaphore::new(4));
        let current_limit = Arc::new(AtomicUsize::new(4));
        store.register_trigger(
            service_instance_id,
            1,
            base,
            semaphore.clone(),
            current_limit.clone(),
        );
        let _held_one = semaphore
            .try_acquire()
            .expect("first in-flight permit should be acquired");
        let _held_two = semaphore
            .try_acquire()
            .expect("second in-flight permit should be acquired");
        let _held_three = semaphore
            .try_acquire()
            .expect("third in-flight permit should be acquired");

        assert_eq!(
            store.reconcile_effective_concurrency(service_instance_id, 1, 1),
            3
        );
        assert_eq!(current_limit.load(Ordering::Relaxed), 3);
        assert_eq!(semaphore.available_permits(), 0);
    }

    #[test]
    fn concurrent_requests_converge_without_permit_counter_split() {
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(710));
        let base = TriggerBasePolicy {
            restart_policy: RestartPolicy::for_testing(),
            scaling: Some(
                ScalingPolicy::builder()
                    .initial_concurrency(1)
                    .max_concurrency(4)
                    .build(),
            ),
        };
        let store = Arc::new(TriggerPolicyOverlayStore::default());
        let semaphore = Arc::new(Semaphore::new(1));
        let current_limit = Arc::new(AtomicUsize::new(1));
        store.register_trigger(
            service_instance_id,
            1,
            base,
            semaphore.clone(),
            current_limit.clone(),
        );

        std::thread::scope(|scope| {
            for limit in [2, 3, 4] {
                let store = store.clone();
                scope.spawn(move || {
                    let overlay =
                        TriggerPolicyOverlay::builder("concurrent desired", Duration::from_secs(1))
                            .concurrency_limit(limit)
                            .build()
                            .expect("overlay shape should be valid");
                    store
                        .request_overlay(service_instance_id, 1, overlay)
                        .expect("overlay should be accepted");
                    store.reconcile_effective_concurrency(service_instance_id, 1, 1);
                });
            }
        });

        assert_eq!(
            current_limit.load(Ordering::Relaxed),
            semaphore.available_permits()
        );
        assert!((1..=4).contains(&current_limit.load(Ordering::Relaxed)));
    }

    #[test]
    fn clearing_overlay_requires_reason() {
        let store = TriggerPolicyOverlayStore::default();
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(9));
        let base = TriggerBasePolicy {
            restart_policy: RestartPolicy::for_testing(),
            scaling: None,
        };
        store.register_trigger(
            service_instance_id,
            1,
            base,
            Arc::new(Semaphore::new(1)),
            Arc::new(AtomicUsize::new(1)),
        );

        assert_eq!(
            store.clear_overlay(service_instance_id, 1, " "),
            Err(TriggerPolicyOverlayError::EmptyReason)
        );
    }
}
