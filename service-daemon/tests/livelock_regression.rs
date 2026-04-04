//! Livelock regression test.
//!
//! This test ensures that the logging service remains responsive to shutdown signals
//! even when the LogQueue is being flooded by a "noisy" service. It validates the
//! `biased;` prioritized selection fix in the logging layer.

use service_daemon::{Registry, RestartPolicy, ServiceDaemon, service};
use std::time::{Duration, Instant};
use tokio::time::timeout;

/// A "noisy" service that generates logs as fast as possible.
/// It intentionally ignores `is_shutdown()` to simulate a non-cooperative service.
#[service(tags = ["noisy"])]
async fn noisy_service() -> anyhow::Result<()> {
    service_daemon::done();
    loop {
        // High-frequency logging to flood the LogQueue
        tracing::info!("SPAMMING_LOG_MESSAGE_TO_TEST_LIVELOCK_STABILITY");
        // Yield to allow other tasks (like the runner and log_service) to progress
        tokio::task::yield_now().await;
    }
}

/// A normal service that waits for shutdown.
#[service(tags = ["normal"])]
async fn normal_service() -> anyhow::Result<()> {
    service_daemon::done();
    while !service_daemon::is_shutdown() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

#[tokio::test]
async fn test_livelock_shutdown_responsiveness() -> anyhow::Result<()> {
    // 1. Initialize daemon with multiple noisy services to maximize pressure
    let mut daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("noisy")
                .with_tag("normal")
                .with_tag("__log__")
                .build(),
        )
        .with_restart_policy(RestartPolicy::for_testing())
        .build();

    let cancel = daemon.cancel_token();

    // 2. Run the daemon
    daemon.run().await;

    // 3. Let it run briefly to saturate the log queue
    tokio::time::sleep(Duration::from_millis(500)).await;

    println!(">>> Triggering shutdown while log queue is flooded...");
    let start = Instant::now();
    cancel.cancel();

    // 4. Wait for shutdown with a strict timeout.
    // With prioritized selection, it should exit almost immediately (within 2s including test sleeps).
    let result = timeout(Duration::from_secs(10), daemon.wait()).await;

    let elapsed = start.elapsed();
    println!(">>> Shutdown finished in {:?}", elapsed);

    match result {
        Ok(res) => {
            println!(">>> SUCCESS: Daemon exited gracefully in {:?}", elapsed);
            res.unwrap();
        }
        Err(_) => {
            panic!("FAILURE: Daemon HANGS (Livelock detected) despite timeout mechanism!");
        }
    }

    Ok(())
}
