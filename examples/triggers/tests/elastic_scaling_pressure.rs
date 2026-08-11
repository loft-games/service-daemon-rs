//! # Queue concurrency pressure test
//!
//! End-to-end integration test that verifies the `TriggerRunner`'s
//! `scale_monitor` background task increases concurrency under sustained
//! pressure.
//!
//! ## Test Strategy
//!
//! 1. Define a broadcast queue (`PressureQueue`) and a trigger handler that
//!    simulates 200ms work per message while tracking peak concurrency.
//! 2. Start a `ServiceDaemon` with all default settings
//!    (`initial_concurrency=1`).
//! 3. A producer task floods the queue with messages.
//! 4. Wait until the `scale_monitor` raises observed handler concurrency.
//! 5. Assert that `peak_concurrency > 1`, proving the scale-up occurred.
//!
//! **Run**: `cargo test -p example-triggers --test elastic_scaling_pressure -- --nocapture`

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use service_daemon::Registry;
use service_daemon::ServiceDaemon;
use service_daemon::provider;
use service_daemon::trigger;

fn isolated_registry() -> Registry {
    Registry::builder()
        .with_tag("__test_queue_concurrency_pressure__")
        .build()
}

fn reset_counters() {
    ACTIVE_COUNT.store(0, Ordering::SeqCst);
    PEAK_COUNT.store(0, Ordering::SeqCst);
    COMPLETED_COUNT.store(0, Ordering::SeqCst);
}

async fn wait_until(description: &'static str, mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(6), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect(description);
}

#[allow(unused_imports)]
use service_daemon::TT::*;

// ---------------------------------------------------------------------------
// Concurrency tracking via statics
// ---------------------------------------------------------------------------

/// Current number of concurrently executing handlers.
static ACTIVE_COUNT: AtomicUsize = AtomicUsize::new(0);
/// High-water mark: maximum observed concurrent handlers.
static PEAK_COUNT: AtomicUsize = AtomicUsize::new(0);
/// Total number of completed handler invocations.
static COMPLETED_COUNT: AtomicUsize = AtomicUsize::new(0);

// ---------------------------------------------------------------------------
// Provider: Broadcast queue for pressure injection
// ---------------------------------------------------------------------------

/// A broadcast queue carrying `String` payloads for the pressure test.
#[provider(Queue(String))]
pub struct PressureQueue;

// ---------------------------------------------------------------------------
// Trigger handler: simulates work and tracks concurrency
// ---------------------------------------------------------------------------

/// Handler that simulates 200ms of work per event.
///
/// Uses static atomics to track the maximum number of concurrently
/// running handler instances. Under `initial_concurrency=1`, the
/// scale_monitor should detect saturation and expand capacity.
#[trigger(Queue(PressureQueue), tags = ["__test_queue_concurrency_pressure__"])]
pub async fn pressure_handler(_payload: String) -> anyhow::Result<()> {
    // Increment active count and update peak high-water mark
    let current = ACTIVE_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
    PEAK_COUNT.fetch_max(current, Ordering::SeqCst);

    // Simulate CPU/IO-bound work
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Decrement active and increment completed
    ACTIVE_COUNT.fetch_sub(1, Ordering::SeqCst);
    COMPLETED_COUNT.fetch_add(1, Ordering::SeqCst);

    Ok(())
}

// ---------------------------------------------------------------------------
// Test body
// ---------------------------------------------------------------------------

#[tokio::test]
async fn elastic_scaling_increases_concurrency_under_pressure() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_test_writer()
        .try_init();

    reset_counters();

    let daemon = ServiceDaemon::builder()
        .with_registry(isolated_registry())
        .build();
    let token = daemon.cancel_token();
    daemon.run().await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    let producer = tokio::spawn(async move {
        for i in 0..50 {
            let queue = PressureQueue::resolve().await;
            let _ = queue.push(format!("msg-{}", i));
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok::<(), anyhow::Error>(())
    });

    wait_until("queue pressure should increase handler concurrency", || {
        PEAK_COUNT.load(Ordering::SeqCst) > 1
    })
    .await;

    token.cancel();

    tokio::time::timeout(Duration::from_secs(5), producer)
        .await
        .expect("pressure producer did not finish in time")
        .expect("pressure producer task panicked")?;

    tokio::time::timeout(Duration::from_secs(5), daemon.wait())
        .await
        .expect("pressure test daemon did not shut down in time")
        .expect("pressure test daemon shutdown failed");

    let peak = PEAK_COUNT.load(Ordering::SeqCst);
    let completed = COMPLETED_COUNT.load(Ordering::SeqCst);

    tracing::info!(
        peak_concurrency = peak,
        completed_handlers = completed,
        "Pressure test results"
    );

    assert!(
        peak > 1,
        "expected queue pressure to increase concurrency beyond 1, peak={peak}, completed={completed}"
    );
    assert!(completed > 0, "expected at least one completed handler");

    Ok(())
}
