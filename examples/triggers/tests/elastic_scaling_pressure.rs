//! # Elastic Scaling Pressure Test
//!
//! End-to-end integration test that verifies the `TriggerRunner`'s
//! `scale_monitor` background task automatically increases concurrency
//! under sustained pressure.
//!
//! ## Test Strategy
//!
//! 1. Define a broadcast queue (`PressureQueue`) and a trigger handler that
//!    simulates 200ms work per message while tracking peak concurrency.
//! 2. Start a `ServiceDaemon` with all default settings
//!    (`initial_concurrency=1`).
//! 3. A producer task floods the queue with messages.
//! 4. After sufficient time for the `scale_monitor` to react (which checks
//!    every 1 second), trigger a graceful shutdown.
//! 5. Assert that `peak_concurrency > 1`, proving the scale-up occurred.
//!
//! **Run**: `cargo test -p example-triggers --test elastic_scaling_pressure -- --nocapture`

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use service_daemon::ServiceDaemon;
use service_daemon::TT::*;
use service_daemon::provider;
use service_daemon::trigger;

// ---------------------------------------------------------------------------
// Concurrency tracking via statics
// ---------------------------------------------------------------------------

/// Current number of concurrently executing handlers.
static ACTIVE_COUNT: AtomicUsize = AtomicUsize::new(0);
/// High-water mark: maximum observed concurrent handlers.
static PEAK_COUNT: AtomicUsize = AtomicUsize::new(0);
/// Total number of completed handler invocations.
static COMPLETED_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Reset all counters (call before each test run to avoid cross-pollution).
fn reset_counters() {
    ACTIVE_COUNT.store(0, Ordering::SeqCst);
    PEAK_COUNT.store(0, Ordering::SeqCst);
    COMPLETED_COUNT.store(0, Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// Provider: Broadcast queue for pressure injection
// ---------------------------------------------------------------------------

/// A broadcast queue carrying `String` payloads for the pressure test.
#[provider(default = Queue, item_type = "String")]
pub struct PressureQueue;

// ---------------------------------------------------------------------------
// Trigger handler: simulates work and tracks concurrency
// ---------------------------------------------------------------------------

/// Handler that simulates 200ms of work per event.
///
/// Uses static atomics to track the maximum number of concurrently
/// running handler instances. Under `initial_concurrency=1`, the
/// scale_monitor should detect saturation and expand capacity.
#[trigger(Queue(PressureQueue))]
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
async fn elastic_scaling_increases_concurrency_under_pressure() {
    // -- Setup tracing for debug output --
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_test_writer()
        .try_init();

    reset_counters();

    // -- Build and start the daemon --
    // Default RestartPolicy has initial_concurrency=1, max_concurrency=1024,
    // scale_factor=2, scale_threshold=5 (~83% utilization triggers scale-up).
    let mut daemon = ServiceDaemon::builder().build();
    let token = daemon.cancel_token();
    daemon.run().await;

    // Give the daemon a moment to initialise triggers
    tokio::time::sleep(Duration::from_millis(200)).await;

    // -- Producer: flood the queue with messages --
    // 100ms between sends at 200ms handler time -> queue builds up fast.
    // With initial_concurrency=1, the semaphore is 100% utilized.
    let producer = tokio::spawn(async {
        for i in 0..50 {
            // push() may block momentarily if the broadcaster is full
            let _ = PressureQueue::push(format!("msg-{}", i)).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });

    // -- Let the system run for 5 seconds --
    // The scale_monitor checks every 1s. After ~1-2s it should detect
    // 100% utilization and scale from 1 -> 2 (then possibly 2 -> 4).
    tokio::time::sleep(Duration::from_secs(5)).await;

    // -- Trigger graceful shutdown --
    token.cancel();

    // Wait for producer to finish (it should be done by now)
    let _ = producer.await;

    // Give in-flight handlers a moment to drain
    tokio::time::sleep(Duration::from_millis(500)).await;

    // -- Assert results --
    let peak = PEAK_COUNT.load(Ordering::SeqCst);
    let completed = COMPLETED_COUNT.load(Ordering::SeqCst);

    tracing::info!(
        peak_concurrency = peak,
        completed_handlers = completed,
        "Pressure test results"
    );

    // The scale_monitor should have detected pressure within 1-2 seconds
    // and increased concurrency beyond the initial value of 1.
    assert!(
        peak > 1,
        "Expected elastic scaling to increase concurrency beyond \
         initial_concurrency=1, but peak was {}. Completed: {}",
        peak,
        completed
    );

    tracing::info!(
        "Pressure test PASSED: peak_concurrency={}, completed={}",
        peak,
        completed
    );
}
