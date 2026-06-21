//! Trigger host templates for controller-style connection event sources.
//!
//! `ControllerHost` is the policy layer between a long-lived connection handle
//! and the framework trigger runner. It does not parse bytes; it only turns
//! connection events into trigger lifecycle transitions.

use crate::adapter::connection::{ConnectionHandle, DeviceEvent};
use crate::models::controller::ControllerEvent;
use futures::future::BoxFuture;
use service_daemon::{TriggerHost, TriggerTransition};
use std::sync::Arc;
use tracing::info;

pub struct ControllerHost {
    stop_after_final_event: bool,
    closed_after_final_event: bool,
}

impl ControllerHost {
    fn transition_from_event(&mut self, event: DeviceEvent) -> TriggerTransition<ControllerEvent> {
        let event = ControllerEvent::from(event);
        if matches!(event, ControllerEvent::Completed) {
            self.stop_after_final_event = true;
        }
        TriggerTransition::Next(event, None)
    }
}

impl TriggerHost<ConnectionHandle> for ControllerHost {
    type Payload = ControllerEvent;

    fn setup(target: Arc<ConnectionHandle>) -> BoxFuture<'static, anyhow::Result<Self>> {
        Box::pin(async move {
            target.connect().await;
            info!("controller connection host initialized");
            Ok(Self {
                stop_after_final_event: false,
                closed_after_final_event: false,
            })
        })
    }

    fn handle_step<'a>(
        &'a mut self,
        target: &'a Arc<ConnectionHandle>,
    ) -> BoxFuture<'a, TriggerTransition<Self::Payload>> {
        Box::pin(async move {
            if self.stop_after_final_event {
                if !self.closed_after_final_event {
                    target.close().await;
                    self.closed_after_final_event = true;
                    info!("controller connection host closed after final event");
                }
                service_daemon::wait_shutdown().await;
                return TriggerTransition::Stop;
            }

            loop {
                match target.recv_event().await {
                    Ok(Some(event)) => {
                        let is_completed = matches!(event, DeviceEvent::Completed);
                        let transition = self.transition_from_event(event);
                        if is_completed && !self.closed_after_final_event {
                            target.close().await;
                            self.closed_after_final_event = true;
                            info!("controller connection host closed after final event");
                        }
                        return transition;
                    }
                    Ok(None) => {
                        if !service_daemon::sleep(std::time::Duration::from_millis(10)).await {
                            return TriggerTransition::Stop;
                        }
                    }
                    Err(error) => {
                        tracing::error!(%error, "controller connection event decode failed");
                        return TriggerTransition::Next(
                            ControllerEvent::ProtocolError {
                                sequence: 0,
                                message: error.to_string(),
                            },
                            None,
                        );
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::adapter::codec::FrameCodec;
    use crate::adapter::connection::{ConnectionHandle, DeviceConnection};
    use crate::models::controller::ControllerEvent;
    use crate::trigger_templates::ControllerHost;
    use service_daemon::{TriggerHost, TriggerTransition};
    use std::sync::Arc;

    #[tokio::test]
    async fn controller_host_dispatches_domain_reload_and_completed_before_stop()
    -> anyhow::Result<()> {
        let connection = Arc::new(ConnectionHandle::new(DeviceConnection::scripted()));
        let mut host =
            <ControllerHost as TriggerHost<ConnectionHandle>>::setup(connection.clone()).await?;

        let first =
            <ControllerHost as TriggerHost<ConnectionHandle>>::handle_step(&mut host, &connection)
                .await;
        assert!(matches!(
            first,
            TriggerTransition::Next(
                ControllerEvent::Measurement {
                    sequence: 0,
                    value: 40
                },
                None
            )
        ));

        let _second =
            <ControllerHost as TriggerHost<ConnectionHandle>>::handle_step(&mut host, &connection)
                .await;
        let interrupted =
            <ControllerHost as TriggerHost<ConnectionHandle>>::handle_step(&mut host, &connection)
                .await;
        assert!(matches!(
            interrupted,
            TriggerTransition::Next(ControllerEvent::LinkInterrupted { sequence: 1 }, None)
        ));

        let recovered =
            <ControllerHost as TriggerHost<ConnectionHandle>>::handle_step(&mut host, &connection)
                .await;
        assert!(matches!(
            recovered,
            TriggerTransition::Next(
                ControllerEvent::LinkRecovered {
                    sequence: 1,
                    reconnects: 1
                },
                None
            )
        ));

        let recovered_measurement =
            <ControllerHost as TriggerHost<ConnectionHandle>>::handle_step(&mut host, &connection)
                .await;
        assert!(matches!(
            recovered_measurement,
            TriggerTransition::Next(
                ControllerEvent::Measurement {
                    sequence: 3,
                    value: 43
                },
                None
            )
        ));

        let reload =
            <ControllerHost as TriggerHost<ConnectionHandle>>::handle_step(&mut host, &connection)
                .await;
        assert!(matches!(
            reload,
            TriggerTransition::Next(ControllerEvent::ReloadRequested { sequence: 4 }, None)
        ));

        let completed =
            <ControllerHost as TriggerHost<ConnectionHandle>>::handle_step(&mut host, &connection)
                .await;
        assert!(matches!(
            completed,
            TriggerTransition::Next(ControllerEvent::Completed, None)
        ));
        assert_eq!(
            connection.state().await,
            crate::adapter::connection::ConnectionState::Closed
        );

        Ok(())
    }

    #[tokio::test]
    async fn controller_host_dispatches_protocol_error_event_without_clean_stop()
    -> anyhow::Result<()> {
        let connection = Arc::new(ConnectionHandle::new(DeviceConnection::new(
            Default::default(),
        )));
        connection.connect().await;
        connection
            .push_inbound_frame(FrameCodec::encode(&[0xff]))
            .await;
        let mut host =
            <ControllerHost as TriggerHost<ConnectionHandle>>::setup(connection.clone()).await?;

        let transition =
            <ControllerHost as TriggerHost<ConnectionHandle>>::handle_step(&mut host, &connection)
                .await;
        assert!(matches!(
            transition,
            TriggerTransition::Next(ControllerEvent::ProtocolError { .. }, None)
        ));
        assert_eq!(
            connection.state().await,
            crate::adapter::connection::ConnectionState::Running
        );

        Ok(())
    }
}
