use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures::FutureExt;
use serde_json::{Value, json};
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry as TraceRegistry};

use crate::{Registry, ServiceDaemon, service};

mod artifacts;
mod evidence;
mod runner;

static START: OnceLock<Instant> = OnceLock::new();
static RECORDS: Mutex<Vec<Value>> = Mutex::new(Vec::new());

fn record(kind: &str, data: Value) {
    RECORDS.lock().unwrap().push(json!({
        "kind": kind, "at_ns": START.get().unwrap().elapsed().as_nanos() as u64,
        "data": data,
    }));
}

struct Trace;

#[derive(Default)]
struct Fields(BTreeMap<String, Value>);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0
            .insert(field.name().into(), json!(format!("{value:?}")));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().into(), json!(value));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().into(), json!(value));
    }
}

impl<S: tracing::Subscriber> Layer<S> for Trace {
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        if event.metadata().target().contains("high_priority") {
            let mut fields = Fields::default();
            event.record(&mut fields);
            record("event", json!(fields.0));
        }
    }
}

#[derive(Clone)]
struct Workload {
    scenario: String,
    competitor: bool,
    block_for: Duration,
}

fn cpu_work(duration: Duration) {
    let until = Instant::now() + duration;
    let mut state = 1u64;
    while Instant::now() < until {
        state = std::hint::black_box(state.wrapping_mul(6364136223846793005).wrapping_add(1));
    }
}

async fn body(input: &Workload) -> anyhow::Result<()> {
    crate::done();
    let identity = crate::core::context::api::current_generation_diagnostics()
        .unwrap()
        .snapshot();
    let instance = identity.service_instance_id.to_string();
    let generation = identity.generation;
    let shard = identity.high_priority_shard_id.map(|shard| shard.0);
    record(
        "generation",
        json!({"instance": instance, "generation": generation, "shard": shard,
        "competitor": input.competitor}),
    );
    while !crate::is_shutdown() {
        if START.get().unwrap().elapsed() < Duration::from_secs(5) {
            tokio::time::sleep(Duration::from_millis(5)).await;
            continue;
        }
        if input.competitor {
            tokio::time::sleep(Duration::from_millis(5)).await;
            cpu_work(input.block_for);
            continue;
        }
        let round = Instant::now();
        if input.scenario == "external_wait" {
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        let started = Instant::now();
        let completed = if input.scenario == "low_benefit" {
            tokio::select! {
                biased;
                completed = crate::sleep(Duration::from_millis(5)) => completed,
                _ = async { cpu_work(input.block_for); std::future::pending::<()>().await } => unreachable!(),
            }
        } else {
            crate::sleep(Duration::from_millis(5)).await
        };
        let elapsed = started.elapsed();
        record(
            "sample",
            json!({"instance": instance, "generation": generation, "shard": shard,
            "completed": completed, "requested_ns": 5_000_000u64,
            "drift_ns": elapsed.saturating_sub(Duration::from_millis(5)).as_nanos() as u64,
            "round_ns": round.elapsed().as_nanos() as u64}),
        );
        if !completed {
            break;
        }
    }
    Ok(())
}

#[service(tags = ["__calibration_standard__"], scheduling = Standard)]
async fn standard_worker(#[input] input: &Workload) -> anyhow::Result<()> {
    body(input).await
}

#[service(tags = ["__calibration_high_priority__"], scheduling = HighPriority)]
async fn high_priority_worker(#[input] input: &Workload) -> anyhow::Result<()> {
    body(input).await
}

async fn run(mode: &str, scenario: &str, seconds: u64, block_for: Duration) -> anyhow::Result<()> {
    let high_priority = mode != "standard";
    let tag = if high_priority {
        "__calibration_high_priority__"
    } else {
        "__calibration_standard__"
    };
    let daemon = ServiceDaemon::builder()
        .with_registry(Registry::builder().with_tag(tag).build())
        .build();
    daemon.run().await;
    let result = std::panic::AssertUnwindSafe(async {
        let resources = daemon.inner.lock().await.resources.clone();
        let handle = crate::core::context::__run_daemon_resources_sync_scope(resources, || {
            if high_priority { crate::service_handle!(high_priority_worker) }
            else { crate::service_handle!(standard_worker) }
        }).await.map_err(|error| anyhow::anyhow!("{error:?}"))?;
        if mode == "fixed" {
            daemon.inner.lock().await.abort_high_priority_policy_loop();
        }
        if high_priority {
            let inner = daemon.inner.lock().await;
            let pool = &inner.high_priority_runtime_pool;
            anyhow::ensure!(pool.total_worker_threads() == 1, "initial capacity must be inferred as one worker");
            record("policy", json!({"control": format!("{:?}", pool.policy()),
                "max_workers": pool.max_worker_threads(), "settle_ms": 2000,
                "high_avg_drift_ms": pool.policy().high_avg_drift_ms(),
                "minimum_completed_samples": pool.policy().minimum_completed_samples(),
                "initial_workers": pool.total_worker_threads()}));
        }
        handle.start(Workload { scenario: scenario.into(), competitor: false, block_for }).await?;
        if scenario == "contention" {
            handle.start(Workload { scenario: scenario.into(), competitor: true, block_for }).await?;
        }
        while START.get().unwrap().elapsed() < Duration::from_secs(seconds) {
            let runtime = daemon.runtime();
            let diagnostics = daemon.inner.lock().await.diagnostics.snapshot();
            record("probes", json!({"shards": diagnostics.high_priority_shards.iter().map(|shard| json!({
                "id": shard.shard_id.0, "pressure": format!("{:?}", shard.pressure_state),
                "completed": shard.recent_runtime_probe.completed,
                "avg_drift_ms": shard.recent_runtime_probe.avg_drift_ms})).collect::<Vec<_>>()}));
            record("resources", json!({"workers": runtime.high_priority_shards.iter().map(|shard| shard.worker_threads).sum::<usize>(),
                "shards": runtime.high_priority_shards.iter().map(|shard| json!({"id": shard.shard_id.0,
                    "workers": shard.worker_threads, "active": shard.active_generations})).collect::<Vec<_>>()}));
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Ok::<_, anyhow::Error>(())
    }).catch_unwind().await;
    record("measurement_end", json!({}));
    daemon.shutdown();
    let cleanup = tokio::time::timeout(Duration::from_secs(10), daemon.wait()).await;
    record("cleanup", json!({"ok": matches!(cleanup, Ok(Ok(())))}));
    anyhow::ensure!(matches!(cleanup, Ok(Ok(()))), "daemon cleanup failed");
    match result {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

#[test]
#[ignore = "production-default case; invoked by the Rust calibration runner tests"]
fn calibration_case() -> anyhow::Result<()> {
    let mode = std::env::var("SD_CALIBRATION_MODE")?;
    let scenario = std::env::var("SD_CALIBRATION_SCENARIO")?;
    let output = std::env::var("SD_CALIBRATION_OUTPUT")?;
    let seconds: u64 = std::env::var("SD_CALIBRATION_SECONDS")?.parse()?;
    let block_ms: u64 = match std::env::var("SD_CALIBRATION_BLOCK_MS") {
        Ok(value) => value.parse()?,
        Err(std::env::VarError::NotPresent) => 250,
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!((1..=1000).contains(&block_ms), "invalid blocking duration");
    anyhow::ensure!(
        ["standard", "fixed", "adaptive"].contains(&mode.as_str()),
        "invalid mode"
    );
    anyhow::ensure!(
        ["healthy", "contention", "low_benefit", "external_wait"].contains(&scenario.as_str()),
        "invalid scenario"
    );
    anyhow::ensure!(seconds >= 6, "duration must include warmup");
    START.set(Instant::now()).unwrap();
    tracing::subscriber::set_global_default(TraceRegistry::default().with(Trace))?;
    record(
        "case",
        json!({"schema": 1, "mode": mode, "scenario": scenario, "seconds": seconds,
        "warmup_seconds": 5, "build": if cfg!(debug_assertions) { "debug" } else { "release" },
        "available_parallelism": std::thread::available_parallelism()?.get(),
        "body_workers": 1, "control_workers": 1, "blocking_work_ms": block_ms}),
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(run(
            &mode,
            &scenario,
            seconds,
            Duration::from_millis(block_ms),
        ))
    }));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    serde_json::to_writer(file, &*RECORDS.lock().unwrap())?;
    match outcome {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}
