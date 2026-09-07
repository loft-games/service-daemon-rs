use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::observation::SleepWindowSnapshot;
use crate::models::{HighPriorityShardId, ServiceInstanceId};

#[derive(Clone, Copy, Debug)]
pub(crate) struct ServiceSample {
    pub instance: ServiceInstanceId,
    pub generation: u64,
    pub shard: HighPriorityShardId,
    pub window: SleepWindowSnapshot,
    pub at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EffectKind {
    Improved,
    PressureCleared,
    LowBenefit,
    PausedLowBenefit,
    PlacementUnchanged,
    TimedOut,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Effect {
    pub before: ServiceSample,
    pub after: Option<ServiceSample>,
    pub target: HighPriorityShardId,
    pub worker_threads: usize,
    pub kind: EffectKind,
}

pub(crate) enum Decision {
    Wait,
    Candidate(ServiceSample),
    Evaluated(Effect),
}

#[derive(Default)]
struct Cursor {
    generation: u64,
    sequence: u64,
    since: Option<Instant>,
    pressure_windows: u32,
    low_benefit: u32,
    pause: Option<Effect>,
}

struct Intervention {
    baseline: ServiceSample,
    target: HighPriorityShardId,
    worker_threads: usize,
    requested_at: Instant,
}

#[derive(Default)]
pub(crate) struct FeedbackController {
    cursors: HashMap<ServiceInstanceId, Cursor>,
    pending: Option<Intervention>,
}

impl FeedbackController {
    pub(crate) fn external_reload(&mut self, instance: ServiceInstanceId) {
        if let Some(cursor) = self.cursors.get_mut(&instance) {
            cursor.pause = None;
            cursor.low_benefit = 0;
            cursor.pressure_windows = 0;
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.baseline.instance == instance)
        {
            self.pending = None;
        }
    }

    pub(crate) fn since(&self, instance: ServiceInstanceId, generation: u64) -> Option<Instant> {
        self.cursors
            .get(&instance)
            .filter(|cursor| cursor.generation == generation)
            .and_then(|cursor| cursor.since)
    }

    pub(crate) fn retain(&mut self, mut exists: impl FnMut(ServiceInstanceId) -> bool) {
        self.cursors.retain(|instance, _| exists(*instance));
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| !exists(pending.baseline.instance))
        {
            self.pending = None;
        }
    }

    pub(crate) fn start(
        &mut self,
        baseline: ServiceSample,
        target: HighPriorityShardId,
        worker_threads: usize,
        now: Instant,
    ) {
        self.pending = Some(Intervention {
            baseline,
            target,
            worker_threads,
            requested_at: now,
        });
    }

    pub(crate) fn expire(&mut self, now: Instant) -> Option<Effect> {
        if !self.pending.as_ref().is_some_and(|pending| {
            now.saturating_duration_since(pending.requested_at) >= Duration::from_secs(120)
        }) {
            return None;
        }
        let pending = self.pending.take()?;
        let effect = Effect {
            before: pending.baseline,
            after: None,
            target: pending.target,
            worker_threads: pending.worker_threads,
            kind: EffectKind::TimedOut,
        };
        self.cursors
            .entry(pending.baseline.instance)
            .or_default()
            .pause = Some(effect);
        Some(effect)
    }

    pub(crate) fn observe(
        &mut self,
        sample: ServiceSample,
        minimum_samples: u64,
        threshold_ns: u64,
        pressure_windows: u32,
    ) -> Decision {
        let cursor = self.cursors.entry(sample.instance).or_default();
        let is_subject = self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.baseline.instance == sample.instance);
        if cursor.generation != sample.generation {
            cursor.generation = sample.generation;
            cursor.sequence = 0;
            cursor.since = None;
            cursor.pressure_windows = 0;
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| is_subject && sample.generation <= pending.baseline.generation)
        {
            return Decision::Wait;
        }
        if sample.window.completed < minimum_samples
            || sample.window.sequence.saturating_sub(cursor.sequence) < minimum_samples
        {
            return Decision::Wait;
        }
        cursor.sequence = sample.window.sequence;
        cursor.since = Some(sample.at);
        let pressured = sample.window.mean_drift_ns >= threshold_ns;
        if is_subject {
            let baseline = self
                .pending
                .as_ref()
                .expect("pending intervention subject")
                .baseline
                .window;
            let comparable = baseline
                .mean_requested_ns
                .abs_diff(sample.window.mean_requested_ns)
                <= baseline.mean_requested_ns.max(1) / 4;
            if !comparable {
                return Decision::Wait;
            }
            let pending = self.pending.take().expect("pending intervention subject");
            let improved = u128::from(sample.window.mean_drift_ns) * 100
                <= u128::from(baseline.mean_drift_ns) * 90;
            let kind = if sample.shard == pending.baseline.shard {
                EffectKind::PlacementUnchanged
            } else if !pressured {
                cursor.low_benefit = 0;
                EffectKind::PressureCleared
            } else if improved {
                cursor.low_benefit = 0;
                EffectKind::Improved
            } else {
                cursor.low_benefit += 1;
                if cursor.low_benefit >= 2 {
                    EffectKind::PausedLowBenefit
                } else {
                    EffectKind::LowBenefit
                }
            };
            cursor.pressure_windows = 0;
            let effect = Effect {
                before: pending.baseline,
                after: Some(sample),
                target: pending.target,
                worker_threads: pending.worker_threads,
                kind,
            };
            if matches!(
                kind,
                EffectKind::PlacementUnchanged | EffectKind::PausedLowBenefit
            ) {
                cursor.pause = Some(effect);
            }
            return Decision::Evaluated(effect);
        }
        if !pressured {
            cursor.pressure_windows = 0;
            cursor.low_benefit = 0;
            cursor.pause = None;
            return Decision::Wait;
        }
        if cursor.pause.is_some() || self.pending.is_some() {
            return Decision::Wait;
        }
        cursor.pressure_windows = cursor.pressure_windows.saturating_add(1);
        if cursor.pressure_windows >= pressure_windows {
            Decision::Candidate(sample)
        } else {
            Decision::Wait
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(now: Instant, generation: u64, drift: u64) -> ServiceSample {
        ServiceSample {
            instance: ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
            generation,
            shard: HighPriorityShardId(generation - 1),
            window: SleepWindowSnapshot {
                sequence: 8,
                completed: 8,
                mean_drift_ns: drift,
                mean_requested_ns: 1_000_000,
            },
            at: now,
        }
    }

    #[test]
    fn timed_out_intervention_late_generation_does_not_resume_expansion() {
        let now = Instant::now();
        let mut controller = FeedbackController::default();
        let baseline = sample(now, 1, 200);
        controller.observe(baseline, 8, 100, 1);
        controller.start(baseline, HighPriorityShardId(1), 2, now);
        let late = now + Duration::from_secs(121);
        assert!(matches!(
            controller.expire(late),
            Some(Effect {
                kind: EffectKind::TimedOut,
                ..
            })
        ));
        for sequence in [8, 16, 24] {
            let mut after = sample(late, 2, 200);
            after.window.sequence = sequence;
            assert!(matches!(
                controller.observe(after, 8, 100, 1),
                Decision::Wait
            ));
            assert!(controller.expire(late).is_none());
        }
    }

    #[test]
    fn timed_out_pause_requires_healthy_evidence_or_explicit_external_reload() {
        for external in [false, true] {
            let now = Instant::now();
            let mut controller = FeedbackController::default();
            let baseline = sample(now, 1, 200);
            controller.observe(baseline, 8, 100, 1);
            controller.start(baseline, HighPriorityShardId(1), 2, now);
            let late = now + Duration::from_secs(121);
            controller.expire(late).unwrap();
            let pause = controller.cursors[&baseline.instance].pause.unwrap();
            assert_eq!(pause.before.generation, 1);
            assert_eq!(pause.target, HighPriorityShardId(1));
            assert_eq!(pause.kind, EffectKind::TimedOut);
            let after = sample(late, 2, 200);
            assert!(matches!(
                controller.observe(after, 8, 100, 1),
                Decision::Wait
            ));
            if external {
                controller.external_reload(baseline.instance);
            } else {
                let mut healthy = after;
                healthy.window.sequence = 16;
                healthy.window.mean_drift_ns = 50;
                assert!(matches!(
                    controller.observe(healthy, 8, 100, 1),
                    Decision::Wait
                ));
            }
            let mut fresh = after;
            fresh.window.sequence = 24;
            assert!(matches!(
                controller.observe(fresh, 8, 100, 1),
                Decision::Candidate(_)
            ));
            assert!(controller.cursors[&baseline.instance].pause.is_none());
        }
    }

    #[test]
    fn timed_out_pause_is_instance_local_and_removed_with_instance() {
        let now = Instant::now();
        let mut controller = FeedbackController::default();
        let baseline = sample(now, 1, 200);
        controller.start(baseline, HighPriorityShardId(1), 2, now);
        controller.expire(now + Duration::from_secs(120)).unwrap();
        let other = ServiceSample {
            instance: ServiceInstanceId::new(uuid::Uuid::from_u128(2)),
            ..baseline
        };
        assert!(matches!(
            controller.observe(other, 8, 100, 1),
            Decision::Candidate(_)
        ));
        controller.retain(|id| id != baseline.instance);
        assert!(!controller.cursors.contains_key(&baseline.instance));
        assert!(controller.pending.is_none());
    }

    #[test]
    fn stale_samples_and_pending_generations_cannot_trigger_more_interventions() {
        let now = Instant::now();
        let mut controller = FeedbackController::default();
        let baseline = sample(now, 1, 200);
        assert!(matches!(
            controller.observe(baseline, 8, 100, 2),
            Decision::Wait
        ));
        assert!(matches!(
            controller.observe(baseline, 8, 100, 2),
            Decision::Wait
        ));
        let fresh = ServiceSample {
            window: SleepWindowSnapshot {
                sequence: 16,
                ..baseline.window
            },
            ..baseline
        };
        assert!(matches!(
            controller.observe(fresh, 8, 100, 2),
            Decision::Candidate(_)
        ));
        controller.start(fresh, HighPriorityShardId(1), 2, now);
        assert!(matches!(
            controller.observe(fresh, 8, 100, 1),
            Decision::Wait
        ));
    }

    #[test]
    fn sustained_low_benefit_pauses_even_while_pressure_remains() {
        let now = Instant::now();
        let mut controller = FeedbackController::default();
        let first = sample(now, 1, 200);
        controller.observe(first, 8, 100, 1);
        controller.start(first, HighPriorityShardId(1), 2, now);
        let second = sample(now, 2, 195);
        assert!(matches!(
            controller.observe(second, 8, 100, 1),
            Decision::Evaluated(Effect {
                kind: EffectKind::LowBenefit,
                ..
            })
        ));
        controller.start(second, HighPriorityShardId(2), 3, now);
        let third = sample(now, 3, 194);
        assert!(matches!(
            controller.observe(third, 8, 100, 1),
            Decision::Evaluated(Effect {
                kind: EffectKind::PausedLowBenefit,
                ..
            })
        ));
        let fresh = ServiceSample {
            window: SleepWindowSnapshot {
                sequence: 16,
                ..third.window
            },
            ..third
        };
        assert!(matches!(
            controller.observe(fresh, 8, 100, 1),
            Decision::Wait
        ));
    }

    #[test]
    fn improvement_clearing_and_insufficient_evidence_are_distinct() {
        for (drift, kind) in [
            (150, EffectKind::Improved),
            (50, EffectKind::PressureCleared),
            (250, EffectKind::LowBenefit),
        ] {
            let now = Instant::now();
            let mut controller = FeedbackController::default();
            let baseline = sample(now, 1, 200);
            controller.start(baseline, HighPriorityShardId(1), 2, now);
            let after = sample(now, 2, drift);
            let insufficient = ServiceSample {
                window: SleepWindowSnapshot {
                    completed: 0,
                    ..after.window
                },
                ..after
            };
            assert!(matches!(
                controller.observe(insufficient, 8, 100, 1),
                Decision::Wait
            ));
            assert!(
                matches!(controller.observe(after, 8, 100, 1), Decision::Evaluated(effect) if effect.kind == kind)
            );
        }
    }

    #[test]
    fn incomparable_window_waits_for_fresh_comparable_evidence() {
        let now = Instant::now();
        let mut controller = FeedbackController::default();
        let baseline = sample(now, 1, 200);
        controller.start(baseline, HighPriorityShardId(1), 2, now);
        let after = sample(now, 2, 150);
        let changed = ServiceSample {
            window: SleepWindowSnapshot {
                mean_requested_ns: 2_000_000,
                ..after.window
            },
            ..after
        };
        assert!(matches!(
            controller.observe(changed, 8, 100, 1),
            Decision::Wait
        ));
        assert!(matches!(
            controller.observe(after, 8, 100, 1),
            Decision::Wait
        ));
        let comparable = ServiceSample {
            window: SleepWindowSnapshot {
                sequence: 16,
                ..after.window
            },
            ..after
        };
        assert!(matches!(
            controller.observe(comparable, 8, 100, 1),
            Decision::Evaluated(Effect {
                kind: EffectKind::Improved,
                ..
            })
        ));
    }

    #[test]
    fn cancelled_subject_unblocks_other_instances_but_generation_gap_does_not() {
        let now = Instant::now();
        let mut controller = FeedbackController::default();
        let baseline = sample(now, 1, 200);
        controller.start(baseline, HighPriorityShardId(1), 2, now);
        controller.retain(|_| true);
        let other = ServiceSample {
            instance: ServiceInstanceId::new(uuid::Uuid::from_u128(2)),
            ..baseline
        };
        assert!(matches!(
            controller.observe(other, 8, 100, 1),
            Decision::Wait
        ));
        controller.retain(|id| id != baseline.instance);
        let fresh = ServiceSample {
            window: SleepWindowSnapshot {
                sequence: 16,
                ..other.window
            },
            ..other
        };
        assert!(matches!(
            controller.observe(fresh, 8, 100, 1),
            Decision::Candidate(_)
        ));
    }
}
