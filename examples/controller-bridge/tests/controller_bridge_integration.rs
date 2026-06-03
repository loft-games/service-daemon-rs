//! Integration tests for the Controller Bridge example.

use example_controller_bridge as _;
use example_controller_bridge::adapter::connection::{
    ConnectionHandle, ConnectionState, DeviceCommand, DeviceConnection, DeviceEvent, DeviceReply,
};
use example_controller_bridge::adapter::protocol::ProtocolCodec;
use example_controller_bridge::models::controller::{
    ControllerCommandRequest, ControllerReplyHandle, ControllerStatus,
};
use example_controller_bridge::providers::ControllerCommandQueue;
use example_controller_bridge::services::controller::{
    controller_event_stats_snapshot, reset_controller_event_stats, send_controller_command,
    send_controller_command_with_timeout,
};
use service_daemon::{RestartPolicy, ServiceDaemon};
use std::sync::Arc;
use std::time::{Duration, Instant};

static INTEGRATION_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn reset_shared_controller_state() {
    reset_controller_event_stats().await;
    {
        let lock = ControllerStatus::resolve_rwlock().await;
        let mut guard = lock.write().await;
        *guard = ControllerStatus::default();
    }
    ConnectionHandle::resolve()
        .await
        .replace_connection(DeviceConnection::scripted())
        .await;
}

async fn wait_until<F, Fut>(
    description: &str,
    timeout: Duration,
    mut predicate: F,
) -> anyhow::Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<bool>>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if predicate().await? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for {description}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn daemon_dispatches_full_controller_script_and_status_watch() -> anyhow::Result<()> {
    let _guard = INTEGRATION_TEST_LOCK.lock().await;
    let _ = service_daemon::try_init_logging();
    reset_shared_controller_state().await;

    let mut daemon = ServiceDaemon::builder()
        .with_restart_policy(RestartPolicy::for_testing())
        .build();
    let cancel = daemon.cancel_token();

    daemon.run().await;
    wait_until(
        "completed controller script",
        Duration::from_secs(2),
        || async {
            let stats = controller_event_stats_snapshot().await;
            Ok(stats.measurements == 3
                && stats.interruptions == 1
                && stats.recoveries == 1
                && stats.reloads == 1
                && stats.completions == 1)
        },
    )
    .await?;
    wait_until(
        "closed controller status watch",
        Duration::from_secs(2),
        || async {
            let stats = controller_event_stats_snapshot().await;
            let status = ControllerStatus::resolve().await;
            let last_status = stats.last_status.as_ref();
            Ok(status.state == ConnectionState::Closed
                && last_status.is_some_and(|snapshot| snapshot.state == ConnectionState::Closed))
        },
    )
    .await?;

    cancel.cancel();
    daemon.wait().await?;

    let stats = controller_event_stats_snapshot().await;
    assert_eq!(stats.measurements, 3);
    assert_eq!(stats.interruptions, 1);
    assert_eq!(stats.recoveries, 1);
    assert_eq!(stats.reloads, 1);
    assert_eq!(stats.completions, 1);
    assert!(
        stats
            .last_status
            .as_ref()
            .is_some_and(|snapshot| snapshot.state == ConnectionState::Closed),
        "watch trigger should observe the final closed status snapshot"
    );

    let snapshot = ControllerStatus::resolve().await;
    assert_eq!(snapshot.reconnects, 1);
    assert_eq!(snapshot.updated_count, 7);
    assert_eq!(snapshot.state, ConnectionState::Closed);
    assert_eq!(
        ConnectionHandle::resolve().await.state().await,
        ConnectionState::Closed
    );

    Ok(())
}

#[tokio::test]
async fn command_queue_correlates_reply_through_daemon_topology() -> anyhow::Result<()> {
    let _guard = INTEGRATION_TEST_LOCK.lock().await;
    let _ = service_daemon::try_init_logging();
    reset_shared_controller_state().await;
    ConnectionHandle::resolve()
        .await
        .replace_connection(DeviceConnection::new(Default::default()))
        .await;

    let mut daemon = ServiceDaemon::builder()
        .with_restart_policy(RestartPolicy::for_testing())
        .build();
    let cancel = daemon.cancel_token();
    daemon.run().await;

    let connection = ConnectionHandle::resolve().await;
    connection.connect().await;
    let command_queue = ControllerCommandQueue::resolve().await;
    wait_until("command queue subscriber", Duration::from_secs(2), || {
        let command_queue = command_queue.clone();
        async move { Ok(command_queue.receiver_count() > 0) }
    })
    .await?;

    let (reply_handle, reply_rx) = ControllerReplyHandle::new();
    command_queue.push(ControllerCommandRequest {
        command: DeviceCommand::Reload { sequence: 77 },
        reply: reply_handle,
    })?;

    wait_until(
        "command frame to be written",
        Duration::from_secs(2),
        || {
            let connection = connection.clone();
            async move { Ok(connection.outbound_frames().await.len() == 1) }
        },
    )
    .await?;
    let payloads = connection.outbound_payloads().await?;
    assert_eq!(payloads.len(), 1);
    assert_eq!(
        ProtocolCodec::decode_command(&payloads[0])?,
        Some(DeviceCommand::Reload { sequence: 77 })
    );

    connection
        .push_inbound_frame(DeviceConnection::reply_frame(DeviceReply::Reloaded {
            sequence: 77,
        }))
        .await;
    wait_until("command reply stat", Duration::from_secs(2), || async {
        let stats = controller_event_stats_snapshot().await;
        Ok(stats.command_replies == 1)
    })
    .await?;

    let reply = reply_rx.await??;
    assert_eq!(reply, DeviceReply::Reloaded { sequence: 77 });

    cancel.cancel();
    daemon.wait().await?;
    Ok(())
}

#[tokio::test]
async fn command_dispatch_completes_reply_on_send_failure() -> anyhow::Result<()> {
    let connection = Arc::new(ConnectionHandle::new(DeviceConnection::new(
        Default::default(),
    )));
    let (reply_handle, reply_rx) = ControllerReplyHandle::new();
    let request = Arc::new(ControllerCommandRequest {
        command: DeviceCommand::Ping { sequence: 1 },
        reply: reply_handle,
    });

    send_controller_command(request, connection).await?;

    let error = reply_rx
        .await?
        .expect_err("send failure should complete the reply handle");
    assert!(error.to_string().contains("command could not be sent"));
    Ok(())
}

#[tokio::test]
async fn command_dispatch_drop_completes_reply_and_clears_pending() -> anyhow::Result<()> {
    let connection = Arc::new(ConnectionHandle::new(DeviceConnection::new(
        Default::default(),
    )));
    connection.connect().await;
    let (reply_handle, reply_rx) = ControllerReplyHandle::new();
    let request = Arc::new(ControllerCommandRequest {
        command: DeviceCommand::Ping { sequence: 3 },
        reply: reply_handle,
    });
    let task = tokio::spawn(send_controller_command_with_timeout(
        request,
        connection.clone(),
        Duration::from_secs(30),
    ));

    wait_until(
        "pending command registration",
        Duration::from_secs(2),
        || {
            let connection = connection.clone();
            async move { Ok(connection.pending_command_count().await == 1) }
        },
    )
    .await?;
    task.abort();

    let error = reply_rx
        .await?
        .expect_err("aborted command dispatch should complete reply with an error");
    assert!(error.to_string().contains("dropped before reply"));
    wait_until("pending command cleanup", Duration::from_secs(2), || {
        let connection = connection.clone();
        async move { Ok(connection.pending_command_count().await == 0) }
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn command_dispatch_times_out_missing_replies() -> anyhow::Result<()> {
    let connection = Arc::new(ConnectionHandle::new(DeviceConnection::new(
        Default::default(),
    )));
    connection.connect().await;
    let (reply_handle, reply_rx) = ControllerReplyHandle::new();
    let request = Arc::new(ControllerCommandRequest {
        command: DeviceCommand::Ping { sequence: 2 },
        reply: reply_handle,
    });

    send_controller_command_with_timeout(request, connection.clone(), Duration::from_millis(25))
        .await?;

    let error = reply_rx
        .await?
        .expect_err("missing reply should complete with a timeout error");
    assert!(error.to_string().contains("timed out"));
    assert_eq!(connection.pending_command_count().await, 0);
    Ok(())
}

#[tokio::test]
async fn connection_layer_handles_split_frames_and_reconnect_observability() -> anyhow::Result<()> {
    let handle = ConnectionHandle::new(DeviceConnection::new(Default::default()));
    handle.connect().await;

    let frame = DeviceConnection::event_frame(DeviceEvent::Measurement {
        sequence: 9,
        value: 88,
    });
    handle.push_inbound_frame(frame[..2].to_vec()).await;
    handle.push_inbound_frame(frame[2..].to_vec()).await;
    handle.disconnect_next_recv().await;
    handle
        .push_inbound_frame(DeviceConnection::event_frame(DeviceEvent::Measurement {
            sequence: 10,
            value: 89,
        }))
        .await;

    assert!(handle.recv_event().await?.is_none());
    assert_eq!(
        handle.recv_event().await?,
        Some(DeviceEvent::Measurement {
            sequence: 9,
            value: 88
        })
    );
    assert_eq!(
        handle.recv_event().await?,
        Some(DeviceEvent::LinkInterrupted { sequence: 1 })
    );
    assert_eq!(
        handle.recv_event().await?,
        Some(DeviceEvent::LinkRecovered {
            sequence: 1,
            reconnects: 1
        })
    );
    assert_eq!(
        handle.recv_event().await?,
        Some(DeviceEvent::Measurement {
            sequence: 10,
            value: 89
        })
    );
    assert_eq!(handle.reconnects().await, 1);

    Ok(())
}

#[tokio::test]
async fn repeated_daemon_runs_start_from_fresh_scripted_connection() -> anyhow::Result<()> {
    let _guard = INTEGRATION_TEST_LOCK.lock().await;
    for _ in 0..2 {
        reset_shared_controller_state().await;
        let mut daemon = ServiceDaemon::builder()
            .with_restart_policy(RestartPolicy::for_testing())
            .build();
        let cancel = daemon.cancel_token();
        daemon.run().await;
        wait_until("completed repeated run", Duration::from_secs(2), || async {
            let stats = controller_event_stats_snapshot().await;
            Ok(stats.completions == 1)
        })
        .await?;
        cancel.cancel();
        daemon.wait().await?;

        let stats = controller_event_stats_snapshot().await;
        assert_eq!(stats.measurements, 3);
        assert_eq!(stats.completions, 1);
    }

    Ok(())
}
