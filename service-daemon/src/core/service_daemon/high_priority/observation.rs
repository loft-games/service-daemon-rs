use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SleepWindowSnapshot {
    pub sequence: u64,
    pub completed: u64,
    pub mean_drift_ns: u64,
    pub mean_requested_ns: u64,
}

struct Sample {
    completed_at: Instant,
    started_at: Instant,
    drift_ns: u64,
    requested_ns: u64,
}

#[derive(Default)]
pub(crate) struct SleepWindow {
    sequence: u64,
    samples: VecDeque<Sample>,
}

impl SleepWindow {
    pub(crate) fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(crate) fn record(
        &mut self,
        now: Instant,
        requested: Duration,
        elapsed: Duration,
        completed: bool,
    ) {
        if !completed {
            return;
        }
        self.sequence = self.sequence.saturating_add(1);
        if self.samples.len() == 128 {
            self.samples.pop_front();
        }
        self.samples.push_back(Sample {
            completed_at: now,
            started_at: now.checked_sub(elapsed).unwrap_or(now),
            drift_ns: elapsed
                .saturating_sub(requested)
                .as_nanos()
                .min(u64::MAX as u128) as u64,
            requested_ns: requested.as_nanos().min(u64::MAX as u128) as u64,
        });
    }

    pub(crate) fn snapshot(&self, now: Instant, since: Instant) -> SleepWindowSnapshot {
        let mut snapshot = SleepWindowSnapshot {
            sequence: self.sequence,
            ..Default::default()
        };
        let mut drift = 0u128;
        let mut requested = 0u128;
        for sample in &self.samples {
            if sample.started_at < since
                || sample.completed_at > now
                || now.duration_since(sample.completed_at) > Duration::from_secs(30)
            {
                continue;
            }
            snapshot.completed += 1;
            drift += u128::from(sample.drift_ns);
            requested += u128::from(sample.requested_ns);
        }
        if snapshot.completed > 0 {
            snapshot.mean_drift_ns = (drift / u128::from(snapshot.completed)) as u64;
            snapshot.mean_requested_ns = (requested / u128::from(snapshot.completed)) as u64;
        }
        snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn sleep_window_excludes_interruptions_stale_and_pre_boundary_sleeps() {
        let now = Instant::now();
        let mut window = SleepWindow::default();
        window.record(
            now,
            Duration::from_millis(1),
            Duration::from_millis(50),
            true,
        );
        window.record(
            now + Duration::from_secs(2),
            Duration::from_millis(1),
            Duration::from_millis(100),
            false,
        );
        window.record(
            now + Duration::from_secs(3),
            Duration::from_micros(100),
            Duration::from_micros(150),
            true,
        );
        let sample = window.snapshot(now + Duration::from_secs(3), now + Duration::from_secs(1));
        assert_eq!(sample.completed, 1);
        assert_eq!(sample.mean_drift_ns, 50_000);
        assert_eq!(sample.sequence, 2);
        assert_eq!(
            window
                .snapshot(now + Duration::from_secs(60), now)
                .completed,
            0
        );
    }

    #[test]
    fn sleep_window_is_bounded_and_reading_does_not_refresh_samples() {
        let now = Instant::now();
        let mut window = SleepWindow::default();
        for _ in 0..300 {
            window.record(now, Duration::ZERO, Duration::ZERO, true);
        }
        let first = window.snapshot(now, now);
        assert_eq!(first.completed, 128);
        assert_eq!(first.sequence, 300);
        assert_eq!(window.snapshot(now, now), first);
    }

    proptest! {
        #[test]
        fn generated_completed_samples_keep_the_window_bounded(
            records in prop::collection::vec((0u64..=u64::MAX, 0u64..=u64::MAX, any::<bool>()), 0..=512)
        ) {
            let now = Instant::now();
            let mut window = SleepWindow::default();
            let expected_count = records.iter().filter(|sample| sample.2).count().min(128);
            for (requested_ns, elapsed_ns, completed) in records {
                window.record(
                    now,
                    Duration::from_nanos(requested_ns),
                    Duration::from_nanos(elapsed_ns),
                    completed,
                );
            }

            let since = window.samples.iter()
                .map(|sample| sample.started_at)
                .min()
                .unwrap_or(now);
            let snapshot = window.snapshot(now, since);
            prop_assert_eq!(snapshot.completed, expected_count as u64);
        }

        #[test]
        fn generated_durations_never_make_means_overflow(
            samples in prop::collection::vec((0u64..=u64::MAX, 0u64..=u64::MAX), 1..=128)
        ) {
            let now = Instant::now();
            let mut window = SleepWindow::default();
            let count = samples.len() as u128;
            let expected_requested = samples.iter()
                .map(|sample| u128::from(sample.0))
                .sum::<u128>() / count;
            let expected_drift = samples.iter()
                .map(|sample| u128::from(sample.1.saturating_sub(sample.0)))
                .sum::<u128>() / count;
            for (requested_ns, elapsed_ns) in samples {
                window.record(
                    now,
                    Duration::from_nanos(requested_ns),
                    Duration::from_nanos(elapsed_ns),
                    true,
                );
            }

            let since = window.samples.iter()
                .map(|sample| sample.started_at)
                .min()
                .unwrap();
            let snapshot = window.snapshot(now, since);
            prop_assert_eq!(u128::from(snapshot.completed), count);
            prop_assert_eq!(u128::from(snapshot.mean_requested_ns), expected_requested);
            prop_assert_eq!(u128::from(snapshot.mean_drift_ns), expected_drift);
        }
    }
}
