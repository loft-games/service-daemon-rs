use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::models::policy::HighPriorityRuntimeControl;
use crate::{Registry, SchedulingAdvisoryProfile, ServiceDaemon, service};

use super::{HighPriorityCapacityPlan, HighPriorityRuntimePool};

static WORKLOAD_RUNNING: AtomicBool = AtomicBool::new(false);
static EXTERNAL_WAIT: AtomicBool = AtomicBool::new(false);
static GENERATION_STARTS: AtomicUsize = AtomicUsize::new(0);
static INTERRUPTED_SLEEPS: AtomicUsize = AtomicUsize::new(0);
static MEASUREMENTS: Mutex<Vec<Measurement>> = Mutex::new(Vec::new());

struct Measurement {
    generation: usize,
    drift_ns: u64,
    round_ns: u64,
}

async fn measured_service() -> anyhow::Result<()> {
    let generation = GENERATION_STARTS.fetch_add(1, Ordering::SeqCst) + 1;
    crate::done();
    while !crate::is_shutdown() {
        if !WORKLOAD_RUNNING.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(5)).await;
            continue;
        }
        let round_started = Instant::now();
        if EXTERNAL_WAIT.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        let requested = Duration::from_millis(5);
        let started = Instant::now();
        if !crate::sleep(requested).await {
            INTERRUPTED_SLEEPS.fetch_add(1, Ordering::SeqCst);
            break;
        }
        let elapsed = started.elapsed();
        MEASUREMENTS.lock().unwrap().push(Measurement {
            generation,
            drift_ns: elapsed.saturating_sub(requested).as_nanos() as u64,
            round_ns: round_started.elapsed().as_nanos() as u64,
        });
    }
    Ok(())
}

async fn competing_service() -> anyhow::Result<()> {
    crate::done();
    while !crate::is_shutdown() {
        tokio::time::sleep(Duration::from_millis(5)).await;
        if !WORKLOAD_RUNNING.load(Ordering::SeqCst) || EXTERNAL_WAIT.load(Ordering::SeqCst) {
            continue;
        }
        let until = Instant::now() + Duration::from_millis(25);
        let mut work = 1u64;
        while Instant::now() < until {
            work = std::hint::black_box(work.wrapping_mul(6364136223846793005).wrapping_add(1));
        }
        std::hint::black_box(work);
    }
    Ok(())
}

#[service(tags = ["__benchmark_standard__"], scheduling = Standard)]
async fn standard_probe() -> anyhow::Result<()> {
    measured_service().await
}

#[service(tags = ["__benchmark_standard__"], scheduling = Standard)]
async fn standard_competitor() -> anyhow::Result<()> {
    competing_service().await
}

#[service(tags = ["__benchmark_high_priority__"], scheduling = HighPriority)]
async fn high_priority_probe() -> anyhow::Result<()> {
    measured_service().await
}

#[service(tags = ["__benchmark_high_priority__"], scheduling = HighPriority)]
async fn high_priority_competitor() -> anyhow::Result<()> {
    competing_service().await
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    Standard,
    FixedHighPriority,
    AdaptiveHighPriority,
}

fn percentile(sorted: &[u64], percent: usize) -> f64 {
    let index = (sorted.len() * percent).div_ceil(100).saturating_sub(1);
    sorted[index] as f64 / 1_000_000.0
}

fn print_measurements(scope: &str, measurements: &[&Measurement]) {
    if measurements.is_empty() {
        println!("{scope}: no completed samples");
        return;
    }
    let mut drift: Vec<_> = measurements.iter().map(|sample| sample.drift_ns).collect();
    let mut round: Vec<_> = measurements.iter().map(|sample| sample.round_ns).collect();
    drift.sort_unstable();
    round.sort_unstable();
    println!(
        "{scope}: samples={} sleep_drift_ms p50={:.3} p95={:.3} p99={:.3} max={:.3}; round_ms p50={:.3} p99={:.3}",
        drift.len(),
        percentile(&drift, 50),
        percentile(&drift, 95),
        percentile(&drift, 99),
        percentile(&drift, 100),
        percentile(&round, 50),
        percentile(&round, 99),
    );
}

async fn run_scenario(mode: Mode, external_wait: bool) {
    WORKLOAD_RUNNING.store(false, Ordering::SeqCst);
    EXTERNAL_WAIT.store(external_wait, Ordering::SeqCst);
    GENERATION_STARTS.store(0, Ordering::SeqCst);
    INTERRUPTED_SLEEPS.store(0, Ordering::SeqCst);
    MEASUREMENTS.lock().unwrap().clear();
    let high_priority = !matches!(mode, Mode::Standard);
    let tag = if high_priority {
        "__benchmark_high_priority__"
    } else {
        "__benchmark_standard__"
    };
    let daemon = ServiceDaemon::builder()
        .with_registry(Registry::builder().with_tag(tag).build())
        .with_scheduling_advisory_profile(SchedulingAdvisoryProfile::disabled())
        .with_test_high_priority_runtime_control(HighPriorityRuntimeControl::for_testing())
        .build();
    if high_priority {
        let mut inner = daemon.inner.lock().await;
        let capacity = HighPriorityCapacityPlan::from_entry_count(1, NonZeroUsize::new(1));
        inner.high_priority_capacity = capacity;
        inner.high_priority_runtime_pool =
            HighPriorityRuntimePool::new(HighPriorityRuntimeControl::for_testing(), capacity);
    }
    daemon.run().await;
    if matches!(mode, Mode::FixedHighPriority) {
        daemon.inner.lock().await.abort_high_priority_policy_loop();
    }
    WORKLOAD_RUNNING.store(true, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(4)).await;
    WORKLOAD_RUNNING.store(false, Ordering::SeqCst);
    let runtime = daemon.runtime();
    let workers: usize = runtime
        .high_priority_shards
        .iter()
        .map(|shard| shard.worker_threads)
        .sum();
    let generations = GENERATION_STARTS.load(Ordering::SeqCst);
    let interrupted = INTERRUPTED_SLEEPS.load(Ordering::SeqCst);
    daemon.shutdown();
    tokio::time::timeout(Duration::from_secs(5), daemon.wait())
        .await
        .expect("benchmark daemon shutdown timeout")
        .expect("benchmark daemon shutdown");
    println!(
        "\nmode={mode:?} scenario={} high_priority_workers={workers} standard_workers=1 control_workers=1 probe_reloads={} interrupted_sleeps_before_shutdown={interrupted}",
        if external_wait {
            "external_wait_40ms"
        } else {
            "shared_worker_cpu_contention_25ms"
        },
        generations.saturating_sub(1),
    );
    let measurements = MEASUREMENTS.lock().unwrap();
    assert!(
        !measurements.is_empty(),
        "no real ServiceSleep samples collected"
    );
    print_measurements("all_generations", &measurements.iter().collect::<Vec<_>>());
    for generation in 1..=generations {
        print_measurements(
            &format!("generation_{generation}"),
            &measurements
                .iter()
                .filter(|sample| sample.generation == generation)
                .collect::<Vec<_>>(),
        );
    }
}

#[test]
#[ignore = "manual real-runtime benchmark; approximately 24 seconds, no absolute latency assertions"]
fn benchmark_service_sleep_feedback_comparison() {
    println!(
        "Manual smoke experiment: one-worker initial service lane; adaptive test policy permits two HP workers. Sleep drift is measured around the real framework sleep API and includes its recording overhead. Generation groups expose reload transients. External wait is intentionally outside ServiceSleep; round latency exposes that blind spot. This is not production-policy tuning or proof of low-benefit convergence."
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("benchmark host runtime");
    runtime.block_on(async {
        for external_wait in [false, true] {
            for mode in [
                Mode::Standard,
                Mode::FixedHighPriority,
                Mode::AdaptiveHighPriority,
            ] {
                run_scenario(mode, external_wait).await;
            }
        }
    });
}
