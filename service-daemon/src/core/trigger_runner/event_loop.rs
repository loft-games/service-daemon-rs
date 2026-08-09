use anyhow::{Error, Result};
use futures::future::{BoxFuture, pending};
use futures::stream::StreamExt;
use std::sync::Arc;
use tracing::info;

use crate::core::context;
use crate::models::trigger::{TriggerHost, TriggerTransition};

use super::TriggerRunner;
use super::dispatch::InFlightDispatches;
use super::drain::TriggerDrainOutcome;

enum TriggerLoopAction {
    Continue,
    Stop,
}

impl<P: Send + Sync + 'static> TriggerRunner<P> {
    /// Run the event loop with a pre-initialized host instance.
    ///
    /// Takes a mutable reference to an already-constructed host. The host's
    /// `handle_step(&mut self, &target)` is called in each iteration, allowing
    /// hosts to maintain state across iterations without relying on `shelve`.
    pub async fn run_with_host<T, H>(&self, host: &mut H, target: Arc<T>) -> anyhow::Result<()>
    where
        T: Send + Sync + 'static,
        H: TriggerHost<T, Payload = P>,
    {
        struct OverlayGenerationGuard {
            store: Option<Arc<crate::core::trigger_policy_overlay::TriggerPolicyOverlayStore>>,
            service_instance_id: crate::models::ServiceInstanceId,
            generation: u64,
        }

        impl Drop for OverlayGenerationGuard {
            fn drop(&mut self) {
                if let Some(store) = &self.store {
                    store.remove_trigger_generation(self.service_instance_id, self.generation);
                }
            }
        }

        let _overlay_generation_guard = OverlayGenerationGuard {
            store: self.policy_overlays.clone(),
            service_instance_id: self.service_instance_id,
            generation: self.generation,
        };
        let mut in_flight = InFlightDispatches::new();
        let mut scale_monitor = self.scale_monitor_future();
        let mut drain_timeout: BoxFuture<'static, TriggerDrainOutcome> =
            Box::pin(pending::<TriggerDrainOutcome>());
        let mut shutdown_draining = false;
        let mut shutdown_drain_completed = 0;
        let mut shutdown_drain_failed = 0;

        loop {
            tokio::select! {
                biased;

                Some(outcome) = in_flight.next() => {
                    match outcome {
                        Ok(()) => {
                            if shutdown_draining {
                                shutdown_drain_completed += 1;
                            }
                        }
                        Err(failure) => {
                            if shutdown_draining {
                                shutdown_drain_failed += 1;
                                Self::record_drain_outcome(TriggerDrainOutcome {
                                    completed: shutdown_drain_completed,
                                    failed: shutdown_drain_failed,
                                    timed_out: false,
                                    residual: in_flight.len(),
                                });
                            }
                            return Err(Error::from(failure));
                        }
                    }
                    if shutdown_draining && in_flight.is_empty() {
                        Self::record_drain_outcome(TriggerDrainOutcome {
                            completed: shutdown_drain_completed,
                            failed: shutdown_drain_failed,
                            timed_out: false,
                            residual: 0,
                        });
                        break;
                    }
                }
                mut drain_outcome = &mut drain_timeout, if shutdown_draining => {
                    drain_outcome.completed = shutdown_drain_completed;
                    drain_outcome.failed = shutdown_drain_failed;
                    Self::record_drain_timeout(self.name, drain_outcome);
                    Self::record_drain_outcome(drain_outcome);
                    break;
                }
                monitor_outcome = &mut scale_monitor => {
                    return monitor_outcome.map_err(Error::from);
                }
                transition = Self::poll_next_event(host, &target, self.name), if !shutdown_draining => {
                    let Some(transition) = transition else {
                        if in_flight.is_empty() {
                            break;
                        }
                        shutdown_draining = true;
                        shutdown_drain_completed = 0;
                        shutdown_drain_failed = 0;
                        drain_timeout = Self::shutdown_drain_timeout(in_flight.len());
                        continue;
                    };

                    match self.handle_transition(transition, &mut in_flight).await? {
                        TriggerLoopAction::Continue => {}
                        TriggerLoopAction::Stop => {
                            if !context::is_shutdown() {
                                Self::drain_in_flight(&mut in_flight).await?;
                            }
                            break;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Wait for the next event from the host, racing against the shutdown signal.
    ///
    /// Returns `None` when a shutdown signal is received (caller should break).
    async fn poll_next_event<T, H>(
        host: &mut H,
        target: &Arc<T>,
        name: &str,
    ) -> Option<TriggerTransition<P>>
    where
        T: Send + Sync + 'static,
        H: TriggerHost<T, Payload = P>,
    {
        tokio::select! {
            biased;

            _ = context::wait_shutdown() => {
                info!("Trigger '{}' received shutdown, exiting", name);
                None
            }
            t = host.handle_step(target) => Some(t),
        }
    }

    /// Dispatch the payload according to the transition type.
    async fn handle_transition(
        &self,
        transition: TriggerTransition<P>,
        in_flight: &mut InFlightDispatches,
    ) -> Result<TriggerLoopAction> {
        match transition {
            TriggerTransition::Next(payload, pre_id) => {
                self.dispatch(payload, pre_id, in_flight).await?;
                Ok(TriggerLoopAction::Continue)
            }
            TriggerTransition::Reload(payload, pre_id) => {
                self.dispatch(payload, pre_id, in_flight).await?;
                info!("Trigger '{}' entering reload-wait state", self.name);
                loop {
                    tokio::select! {
                        biased;

                        Some(outcome) = in_flight.next() => {
                            outcome.map_err(Error::from)?;
                        }
                        _ = context::wait_shutdown() => return Ok(TriggerLoopAction::Stop),
                    }
                }
            }
            TriggerTransition::Stop => {
                info!("Trigger '{}' stopping", self.name);
                Ok(TriggerLoopAction::Stop)
            }
        }
    }
}
