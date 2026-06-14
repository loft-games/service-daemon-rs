use anyhow::{Error, Result};
use futures::future::BoxFuture;
use futures::stream::StreamExt;
use std::time::Duration;
use tracing::warn;

use crate::core::context;
use crate::core::diagnostics::{
    ShutdownBoundaryKind, ShutdownBoundaryOutcomeSnapshot, ShutdownBoundaryResultKind,
    ShutdownResidualActionKind,
};

use super::TriggerRunner;
use super::dispatch::InFlightDispatches;

const DEFAULT_TRIGGER_SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TriggerDrainOutcome {
    pub(super) completed: usize,
    pub(super) failed: usize,
    pub(super) timed_out: bool,
    pub(super) residual: usize,
}

impl TriggerDrainOutcome {
    fn diagnostics_outcome(self) -> ShutdownBoundaryOutcomeSnapshot {
        let (result, action) = if self.timed_out {
            (
                ShutdownBoundaryResultKind::TimedOut,
                ShutdownResidualActionKind::RecordedAndDetached,
            )
        } else {
            (
                ShutdownBoundaryResultKind::Completed,
                ShutdownResidualActionKind::None,
            )
        };

        ShutdownBoundaryOutcomeSnapshot {
            boundary: ShutdownBoundaryKind::TriggerDispatchDrain,
            result,
            action,
            completed: self.completed as u64,
            failed: self.failed as u64,
            residual: self.residual as u64,
        }
    }
}

impl<P: Send + Sync + 'static> TriggerRunner<P> {
    pub(super) async fn drain_in_flight(in_flight: &mut InFlightDispatches) -> Result<()> {
        while let Some(outcome) = in_flight.next().await {
            outcome.map_err(Error::from)?;
        }
        Ok(())
    }

    pub(super) fn shutdown_drain_timeout(
        residual: usize,
    ) -> BoxFuture<'static, TriggerDrainOutcome> {
        Self::shutdown_drain_timeout_for(residual, DEFAULT_TRIGGER_SHUTDOWN_DRAIN_TIMEOUT)
    }

    pub(super) fn shutdown_drain_timeout_for(
        residual: usize,
        timeout: Duration,
    ) -> BoxFuture<'static, TriggerDrainOutcome> {
        Box::pin(async move {
            tokio::time::sleep(timeout).await;
            TriggerDrainOutcome {
                completed: 0,
                failed: 0,
                timed_out: true,
                residual,
            }
        })
    }

    pub(super) fn record_drain_timeout(trigger_name: &str, outcome: TriggerDrainOutcome) {
        warn!(
            trigger = %trigger_name,
            completed = outcome.completed,
            failed = outcome.failed,
            timed_out = outcome.timed_out,
            residual = outcome.residual,
            "Trigger shutdown drain timed out with residual dispatches"
        );
    }

    pub(super) fn record_drain_outcome(outcome: TriggerDrainOutcome) {
        if let Some(diagnostics) = context::current_generation_diagnostics() {
            diagnostics.record_shutdown_boundary(outcome.diagnostics_outcome());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_drain_timeout_reports_residual_dispatches() {
        let outcome = TriggerRunner::<()>::shutdown_drain_timeout_for(3, Duration::ZERO).await;

        assert!(outcome.timed_out);
        assert_eq!(outcome.residual, 3);
        assert_eq!(outcome.completed, 0);
        assert_eq!(outcome.failed, 0);
    }
}
