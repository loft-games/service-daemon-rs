//! Two natural pressure episodes separated by a real empty-shard valley.
use super::{RECORDS, START, Trace, cpu_work, evidence, record};
use crate::core::service_daemon::DaemonInstanceHandle;
use crate::{Registry, ServiceDaemon, service};
use anyhow::{Context, Result, ensure};
use futures::FutureExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};
use tracing_subscriber::{Registry as TraceRegistry, layer::SubscriberExt};

mod ab;
mod cycles;
mod reclaim;

#[derive(Clone, Copy)]
enum Role {
    First,
    Second,
    Round(u8),
    Competitor,
}

#[derive(Clone)]
struct Input {
    phase: Arc<AtomicU8>,
    role: Role,
}

#[service(tags = ["__reuse_experiment__"], scheduling = HighPriority)]
async fn reuse_worker(#[input] input: &Input) -> anyhow::Result<()> {
    crate::done();
    let identity = crate::core::context::api::current_generation_diagnostics()
        .unwrap()
        .snapshot();
    let instance = identity.service_instance_id.to_string();
    let generation = identity.generation;
    let shard = identity.high_priority_shard_id.map(|s| s.0);
    record(
        "generation",
        json!({"instance":instance,"generation":generation,"shard":shard,
        "competitor":matches!(input.role, Role::Competitor)}),
    );
    while !crate::is_shutdown() {
        let phase = input.phase.load(Ordering::Acquire);
        let active = match input.role {
            Role::First => phase == 0,
            Role::Second => phase == 2,
            Role::Round(round) => phase == round * 2,
            Role::Competitor => phase != 1,
        };
        if START.get().unwrap().elapsed() < Duration::from_secs(5) || !active {
            tokio::time::sleep(Duration::from_millis(5)).await;
            continue;
        }
        if matches!(input.role, Role::Competitor) {
            tokio::time::sleep(Duration::from_millis(5)).await;
            cpu_work(Duration::from_millis(400));
            continue;
        }
        let started = Instant::now();
        let completed = crate::sleep(Duration::from_millis(5)).await;
        let elapsed = started.elapsed();
        record(
            "sample",
            json!({"instance":instance,"generation":generation,"shard":shard,
            "completed":completed,"requested_ns":5_000_000u64,
            "drift_ns":elapsed.saturating_sub(Duration::from_millis(5)).as_nanos() as u64,
            "round_ns":elapsed.as_nanos() as u64}),
        );
        if !completed {
            break;
        }
    }
    Ok(())
}

async fn capture(daemon: &DaemonInstanceHandle) {
    let inner = daemon.inner.lock().await;
    let shards = inner.high_priority_runtime_pool.snapshot();
    let diagnostics = inner.diagnostics.snapshot();
    record(
        "resources",
        json!({"workers":inner.high_priority_runtime_pool.total_worker_threads(),
        "shards":shards.iter().map(|s| json!({"id":s.shard_id.0,"active":s.active_generations,
            "assigned":s.assigned_instances,"pressure":format!("{:?}",s.pressure_state)})).collect::<Vec<_>>()}),
    );
    record(
        "probes",
        json!({"shards":diagnostics.high_priority_shards.iter().map(|s|
        json!({"id":s.shard_id.0,"pressure":format!("{:?}",s.pressure_state),
            "completed":s.recent_runtime_probe.completed,"avg_drift_ms":s.recent_runtime_probe.avg_drift_ms})).collect::<Vec<_>>(),
            "lanes":diagnostics.lanes.iter().map(|s|json!({"lane":format!("{:?}",s.runtime_lane),
                "completed":s.recent_runtime_probe.completed,"avg_drift_ms":s.recent_runtime_probe.avg_drift_ms})).collect::<Vec<_>>() }),
    );
}

fn evaluated(instance: &str) -> bool {
    RECORDS.lock().unwrap().iter().any(|r| {
        r["kind"] == "event"
            && r["data"]["message"] == "HighPriority intervention evaluated"
            && r["data"]["service_instance_id"] == instance
    })
}

async fn wait_effect(daemon: &DaemonInstanceHandle, instance: &str) -> Result<()> {
    let started = Instant::now();
    loop {
        capture(daemon).await;
        if evaluated(instance) {
            return Ok(());
        }
        ensure!(
            started.elapsed() < Duration::from_secs(60),
            "no evaluated intervention for {instance}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn observe_for(daemon: &DaemonInstanceHandle, seconds: u64) {
    let until = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < until {
        capture(daemon).await;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn select_instance(records: &[Value], instance: &str) -> Vec<Value> {
    records
        .iter()
        .filter(|r| {
            let data = &r["data"];
            match r["kind"].as_str() {
                Some("sample" | "generation") => data["instance"] == instance,
                Some("event") if data["service_instance_id"].is_string() => {
                    data["service_instance_id"] == instance
                }
                _ => true,
            }
        })
        .cloned()
        .collect()
}

fn save(path: &Path, value: &impl Serialize) -> Result<()> {
    serde_json::to_writer_pretty(
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?,
        value,
    )?;
    Ok(())
}

async fn run_case(output: &Path, arm: Option<ab::Arm>) -> Result<()> {
    run_case_variant(output, arm, None).await
}

async fn run_case_variant(
    output: &Path,
    arm: Option<ab::Arm>,
    reclamation: Option<bool>,
) -> Result<()> {
    let daemon = ServiceDaemon::builder()
        .with_registry(Registry::builder().with_tag("__reuse_experiment__").build())
        .build();
    daemon.run().await;
    let mut checkpoints = Vec::new();
    let result = std::panic::AssertUnwindSafe(async {
        let resources = daemon.inner.lock().await.resources.clone();
        let handle = crate::core::context::__run_daemon_resources_sync_scope(resources,
            || crate::service_handle!(reuse_worker)).await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let capacity = {
            let inner = daemon.inner.lock().await;
            let pool = &inner.high_priority_runtime_pool;
            ensure!(pool.total_worker_threads() == 1, "initial topology differs");
            record("policy",json!({"max_workers":pool.max_worker_threads(),"settle_ms":2000,
                "high_avg_drift_ms":pool.policy().high_avg_drift_ms(),
                "minimum_completed_samples":pool.policy().minimum_completed_samples(),
                "control":format!("{:?}",pool.policy())}));
            pool.max_worker_threads()
        };
        ensure!(capacity == std::thread::available_parallelism()?.get() && [2,3].contains(&capacity), "run with two or three CPU affinity");
        ensure!(arm.is_none() || capacity == 3,"A/B requires the same three-worker budget");
        let phase = Arc::new(AtomicU8::new(0));
        let first = handle.start(Input { phase:phase.clone(), role:Role::First }).await?;
        let second = handle.start(Input { phase:phase.clone(), role:Role::Second }).await?;
        let competitor = handle.start(Input { phase:phase.clone(), role:Role::Competitor }).await?;
        let first_id = first.instance_id().to_string();
        let second_id = second.instance_id().to_string();
        let competitor_id = competitor.instance_id().to_string();
        wait_effect(&daemon, &first_id).await?;
        let first_effect_at = START.get().unwrap().elapsed().as_nanos() as u64;
        phase.store(1, Ordering::Release);
        ensure!(first.remove().await?, "first subject not removed");
        // Real time: expire both production scale and rollover cooldowns, settle probes.
        observe_for(&daemon, 65).await;
        let valley = {
            let inner = daemon.inner.lock().await;
            let pool = &inner.high_priority_runtime_pool;
            let shards = pool.snapshot();
            ensure!(shards.len() == 2 && pool.total_worker_threads() == 2, "first expansion did not leave two shards");
            let empty_nominal: Vec<_> = shards.iter().filter(|s| s.active_generations == 0
                && s.assigned_instances == 0 && format!("{:?}",s.pressure_state) == "Nominal").map(|s|s.shard_id.0).collect();
            ensure!(empty_nominal == vec![1], "newly expanded shard is not idle and nominal");
            ensure!(pool.state().active_on_shard(crate::models::HighPriorityShardId(0)).contains(&(second.instance_id(),1)), "second subject no longer on original generation/shard");
            ensure!(pool.state().next_placements.is_empty() && pool.state().pending_rollovers.is_empty(), "pending placement in valley");
            Valley {workers:2,capacity,existing:shards.iter().map(|s|s.shard_id.0).collect(),empty_nominal}
        };
        let reclamation_data = if let Some(remove) = reclamation {
            Some(reclaim::long_valley(&daemon, remove, first_effect_at).await?)
        } else { None };
        let second_pressure_at = START.get().unwrap().elapsed().as_nanos() as u64;
        checkpoints.push(json!({"first":first_id,"second":second_id,"competitor":competitor_id,
            "first_effect_at_ns":first_effect_at,"second_pressure_at_ns":second_pressure_at,"valley":valley}));
        if let Some(data) = reclamation_data { checkpoints[0]["reclamation"] = data; }
        if let Some(arm) = arm {
            daemon.inner.lock().await.high_priority_runtime_pool.experiment_prefer_idle = arm == ab::Arm::IdleFirst;
            checkpoints[0]["placement_arm"] = serde_json::to_value(arm)?;
        }
        phase.store(2, Ordering::Release);
        if arm.is_some() {
            observe_for(&daemon, ab::PHASE_SECONDS).await;
            ensure!(evaluated(&second_id),"no second intervention within fixed A/B phase");
        } else {
            wait_effect(&daemon, &second_id).await?;
            observe_for(&daemon, 5).await;
        }
        while START.get().unwrap().elapsed() < Duration::from_secs(95) { observe_for(&daemon,1).await; }
        if reclamation.is_some() { record("event",json!({"message":"HighPriority experimental OS second phase ended","os":reclaim::os_sample()?})); }
        let workers = daemon.inner.lock().await.high_priority_runtime_pool.total_worker_threads();
        Ok::<_,anyhow::Error>((first_id,second_id,competitor_id,valley,workers))
    }).catch_unwind().await;
    record("measurement_end", json!({}));
    daemon.shutdown();
    let cleanup = tokio::time::timeout(Duration::from_secs(10), daemon.wait()).await;
    record("cleanup", json!({"ok":matches!(cleanup,Ok(Ok(())))}));
    let records = RECORDS.lock().unwrap().clone();
    save(&output.join("raw.json"), &records)?;
    save(&output.join("checkpoints.json"), &checkpoints)?;
    ensure!(matches!(cleanup, Ok(Ok(()))), "cleanup failed");
    let (first, second, competitor, valley, workers) = match result {
        Ok(r) => r?,
        Err(p) => std::panic::resume_unwind(p),
    };
    let mut summary = validate_case_topology(
        &records,
        &first,
        &second,
        &competitor,
        &valley,
        workers,
        reclamation == Some(true),
    )?;
    if reclamation.is_some() {
        summary["reclamation"] = reclaim::metrics(&records, &checkpoints)?;
    } else if let Some(arm) = arm {
        summary["comparison"] = ab::metrics(&records, &checkpoints, arm)?;
    }
    save(&output.join("summary.json"), &summary)
}

fn validate_case(
    records: &[Value],
    first: &str,
    second: &str,
    competitor: &str,
    valley: &Valley,
    workers: usize,
) -> Result<Value> {
    validate_case_topology(records, first, second, competitor, valley, workers, false)
}

fn validate_case_topology(
    records: &[Value],
    first: &str,
    second: &str,
    competitor: &str,
    valley: &Valley,
    workers: usize,
    reclaimed: bool,
) -> Result<Value> {
    let expected = evidence::Expected {
        mode: "adaptive".into(),
        scenario: "contention".into(),
        seconds: 90,
        blocking_work_ms: 400,
    };
    let mut summaries = Vec::new();
    for instance in [first, second] {
        let selected = select_instance(records, instance);
        let summary = evidence::validate_json(&serde_json::to_vec(&selected)?, &expected, false)?;
        ensure!(
            summary.verdict == evidence::Verdict::Pass
                && summary.requests == 1
                && summary.linked_interventions == 1,
            "each pressure subject needs its own complete beneficial intervention"
        );
        summaries.push(serde_json::to_value(summary)?);
    }
    let request = records
        .iter()
        .find(|r| {
            r["data"]["message"] == "HighPriority resource intervention requested"
                && r["data"]["service_instance_id"] == second
        })
        .context("missing second request")?;
    let target: u64 = request["data"]["target_shard"]
        .as_str()
        .context("missing target")?
        .strip_prefix("hp#")
        .context("bad target")?
        .parse()?;
    let request_at = request["at_ns"].as_u64().context("missing request time")?;
    let before_request = records
        .iter()
        .rev()
        .find(|r| r["kind"] == "resources" && r["at_ns"].as_u64().is_some_and(|t| t < request_at))
        .context("missing pre-request resources")?;
    ensure!(
        request_at - before_request["at_ns"].as_u64().context("missing time")? < 1_000_000_000,
        "stale pre-request resource evidence"
    );
    if reclaimed {
        ensure!(
            before_request["data"]["workers"] == 1
                && before_request["data"]["shards"]
                    .as_array()
                    .is_some_and(|s| s.len() == 1 && s[0]["id"] == 0),
            "reclaimed target still in pool before request"
        );
        ensure!(
            target == 2 && workers == 2,
            "rebuild did not use a fresh shard within restored budget"
        );
    } else {
        ensure!(
            before_request["data"]["shards"]
                .as_array()
                .context("missing shards")?
                .iter()
                .any(|s| s["id"] == 1
                    && s["active"] == 0
                    && s["assigned"] == 0
                    && s["pressure"] == "Nominal"),
            "old shard was not idle and nominal immediately before second request"
        );
    }
    let effect = records
        .iter()
        .find(|r| {
            r["data"]["message"] == "HighPriority intervention evaluated"
                && r["data"]["service_instance_id"] == second
        })
        .context("missing second effect")?;
    let effect_at = effect["at_ns"].as_u64().context("missing effect time")?;
    let pressure_after = records
        .iter()
        .find(|r| {
            r["kind"] == "probes"
                && r["at_ns"].as_u64().is_some_and(|t| t > effect_at)
                && r["data"]["shards"].as_array().is_some_and(|shards| {
                    shards.iter().any(|s| {
                        s["id"] == 0
                            && s["pressure"] == "Pressured"
                            && s["avg_drift_ms"].as_u64().is_some_and(|n| n >= 100)
                    })
                })
        })
        .context("source pressure was not observed after subject improvement")?;
    let competitor_generations: Vec<_> = records
        .iter()
        .filter(|r| r["kind"] == "generation" && r["data"]["instance"] == competitor)
        .collect();
    ensure!(
        competitor_generations.len() == 1 && competitor_generations[0]["data"]["shard"] == 0,
        "competitor did not stay on source"
    );
    let outcome = if reclaimed {
        json!("rebuilt_after_reclaim")
    } else {
        serde_json::to_value(classify(valley, target, workers)?)?
    };
    Ok(
        json!({"outcome":outcome,"valley":valley,"second_target":target,
        "workers_after":workers,"subjects":summaries,"cleanup":true,
        "before_second_request":before_request,"second_effect":effect,"source_pressure_after":pressure_after}),
    )
}

#[test]
#[ignore = "two natural pressure episodes; taskset to 2 or 3 CPUs; set SD_REUSE_CASE"]
fn reuse_case() -> Result<()> {
    let output = std::path::PathBuf::from(std::env::var("SD_REUSE_CASE")?);
    fs::create_dir(&output)?;
    START.set(Instant::now()).unwrap();
    tracing::subscriber::set_global_default(TraceRegistry::default().with(Trace))?;
    record(
        "case",
        json!({"schema":1,"mode":"adaptive","scenario":"contention","seconds":90,
        "blocking_work_ms":400,"build":if cfg!(debug_assertions){"debug"}else{"release"},
        "available_parallelism":std::thread::available_parallelism()?.get()}),
    );
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?
        .block_on(run_case(&output, None))
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PlacementOutcome {
    ReusedEmpty,
    AllocatedDespiteEmpty,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Valley {
    workers: usize,
    capacity: usize,
    existing: Vec<u64>,
    empty_nominal: Vec<u64>,
}

fn classify(valley: &Valley, target: u64, workers_after: usize) -> Result<PlacementOutcome> {
    ensure!(
        valley
            .empty_nominal
            .iter()
            .all(|id| valley.existing.contains(id)),
        "empty shard not in pool"
    );
    ensure!(
        !valley.empty_nominal.is_empty(),
        "no idle nominal shard at second pressure onset"
    );
    ensure!(workers_after <= valley.capacity, "capacity exceeded");
    if valley.empty_nominal.contains(&target) && workers_after == valley.workers {
        return Ok(PlacementOutcome::ReusedEmpty);
    }
    ensure!(
        !valley.existing.contains(&target) && workers_after == valley.workers + 1,
        "neither verified empty reuse nor one-shard allocation"
    );
    Ok(PlacementOutcome::AllocatedDespiteEmpty)
}

#[test]
fn reuse_second_subject_cannot_borrow_first_subject_evidence() {
    let records = vec![
        json!({"kind":"resources","data":{"workers":2}}),
        json!({"kind":"sample","data":{"instance":"first"}}),
        json!({"kind":"generation","data":{"instance":"first"}}),
        json!({"kind":"event","data":{"service_instance_id":"first","message":"HighPriority intervention evaluated"}}),
        json!({"kind":"sample","data":{"instance":"second"}}),
    ];
    let second = select_instance(&records, "second");
    assert_eq!(second, vec![records[0].clone(), records[4].clone()]);
}

fn affinity_cpus() -> Result<Vec<usize>> {
    let status = fs::read_to_string("/proc/self/status")?;
    let list = status
        .lines()
        .find_map(|l| l.strip_prefix("Cpus_allowed_list:"))
        .context("missing affinity")?;
    let mut cpus = Vec::new();
    for part in list.trim().split(',') {
        if let Some((a, b)) = part.split_once('-') {
            cpus.extend(a.parse::<usize>()?..=b.parse::<usize>()?);
        } else {
            cpus.push(part.parse()?);
        }
    }
    ensure!(
        cpus.len() >= 3,
        "three available CPUs required for paired experiment"
    );
    Ok(cpus)
}

#[test]
#[ignore = "archived Linux empty-shard reuse experiment, six cases; fresh SD_REUSE_DIR"]
fn reuse_experiment() -> Result<()> {
    run_experiment(Experiment::Reuse)
}

#[test]
#[ignore = "archived six-cycle experiment, three fresh processes; fresh SD_REUSE_DIR"]
fn cycles_experiment() -> Result<()> {
    run_experiment(Experiment::Cycles)
}

#[test]
#[ignore = "same-budget placement A/B, four pairs; fresh SD_REUSE_DIR"]
fn ab_experiment() -> Result<()> {
    run_experiment(Experiment::IdleFirstComparison)
}

#[test]
#[ignore = "retain vs reclaim/rebuild, four pairs with five-minute valleys; fresh SD_REUSE_DIR"]
fn reclaim_experiment() -> Result<()> {
    run_experiment(Experiment::Reclamation)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Experiment {
    Reuse,
    Cycles,
    IdleFirstComparison,
    Reclamation,
}

fn run_experiment(experiment: Experiment) -> Result<()> {
    use super::artifacts;
    use std::collections::BTreeMap;
    use std::process::{Command, Stdio};
    let cycles = experiment == Experiment::Cycles;
    let comparison = experiment == Experiment::IdleFirstComparison;
    let reclamation = experiment == Experiment::Reclamation;
    let paired = comparison || reclamation;
    let repetitions = if paired { 4 } else { 3 };
    let output = std::path::PathBuf::from(std::env::var("SD_REUSE_DIR")?);
    ensure!(output.is_absolute(), "absolute SD_REUSE_DIR required");
    let cpus = affinity_cpus()?;
    fs::create_dir(&output)?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("missing root")?;
    let sources = artifacts::snapshot(
        root,
        &output.join("sources"),
        &artifacts::source_paths(root)?,
    )?;
    let version = |tool: &str| -> Result<String> {
        let result = Command::new(tool).arg("--version").output()?;
        ensure!(result.status.success(), "version command failed");
        Ok(String::from_utf8(result.stdout)?.trim().into())
    };
    save(
        &output.join("inputs.json"),
        &json!({"schema":1,"sources":sources,"rustc":version("rustc")?,
        "cargo":version("cargo")?,"taskset":version("taskset")?,"available_cpus":cpus,
        "kernel":fs::read_to_string("/proc/sys/kernel/osrelease")?,"preflight_loadavg":fs::read_to_string("/proc/loadavg")?,
        "repetitions":repetitions,"capacities":if cycles || paired {vec![3]} else {vec![2,3]},
        "rounds_per_process":if cycles {6} else {2},
        "placement_comparison":comparison,"comparison_phase_seconds":if paired {Some(ab::PHASE_SECONDS)} else {None},
        "reclamation":reclamation,"blocking_ms":400,"valley_seconds":if reclamation {300} else {65}}),
    )?;
    let build = Command::new("cargo")
        .current_dir(output.join("sources"))
        .args([
            "test",
            "--locked",
            "--release",
            "-p",
            "service-daemon",
            "--features",
            "high-priority",
            "--lib",
            "--no-run",
            "--message-format=json",
            "-j",
            "2",
            "--target-dir",
        ])
        .arg(output.join("build"))
        .output()?;
    fs::write(output.join("build.stdout"), &build.stdout)?;
    fs::write(output.join("build.stderr"), &build.stderr)?;
    ensure!(build.status.success(), "archived build failed");
    let binary = String::from_utf8(build.stdout)?
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find_map(|v| {
            if v["reason"] == "compiler-artifact"
                && v["target"]["name"] == "service_daemon"
                && v["profile"]["test"] == true
            {
                v["executable"].as_str().map(str::to_owned)
            } else {
                None
            }
        })
        .context("missing executable")?;
    let executable = output.join("reuse-test");
    fs::copy(binary, &executable)?;
    let binary_hash = artifacts::hash(&fs::read(&executable)?);
    let mut success = true;
    let mut cases = Vec::new();
    for repetition in 1usize..=repetitions {
        let jobs = if reclamation {
            if repetition.is_multiple_of(2) {
                vec![(3, "reclaim"), (3, "retain")]
            } else {
                vec![(3, "retain"), (3, "reclaim")]
            }
        } else if comparison {
            if repetition.is_multiple_of(2) {
                vec![(3, "idle_first"), (3, "growth_first")]
            } else {
                vec![(3, "growth_first"), (3, "idle_first")]
            }
        } else if cycles {
            vec![(3, "")]
        } else if repetition.is_multiple_of(2) {
            vec![(3, ""), (2, "")]
        } else {
            vec![(2, ""), (3, "")]
        };
        for (capacity, arm) in jobs {
            let name = if paired {
                format!("{repetition}-{arm}")
            } else {
                format!("{repetition}-capacity-{capacity}")
            };
            let affinity = cpus[..capacity]
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(",");
            eprintln!("reuse experiment {name}, affinity {affinity}");
            let start = Instant::now();
            let mut child = Command::new("taskset")
                .args(["--cpu-list", &affinity])
                .arg(&executable)
                .args([
                    "--exact",
                    if reclamation {
                        "core::service_daemon::high_priority::calibration::reuse::reclaim::reclaim_case"
                    } else if comparison {
                        "core::service_daemon::high_priority::calibration::reuse::ab::ab_case"
                    } else if cycles {
                        "core::service_daemon::high_priority::calibration::reuse::cycles::cycles_case"
                    } else {
                        "core::service_daemon::high_priority::calibration::reuse::reuse_case"
                    },
                    "--ignored",
                    "--nocapture",
                ])
                .env("SD_REUSE_CASE", output.join(&name))
                .env("SD_PLACEMENT_ARM",arm)
                .stdout(Stdio::from(fs::File::create(
                    output.join(format!("{name}.stdout")),
                )?))
                .stderr(Stdio::from(fs::File::create(
                    output.join(format!("{name}.stderr")),
                )?))
                .spawn()?;
            let status = loop {
                if let Some(status) = child.try_wait()? {
                    break status;
                }
                if start.elapsed()
                    > Duration::from_secs(if cycles {
                        900
                    } else if reclamation {
                        480
                    } else {
                        240
                    })
                {
                    child.kill()?;
                    break child.wait()?;
                }
                std::thread::sleep(Duration::from_millis(100));
            };
            let replay = if paired && status.success() {
                let replay = Command::new(&executable)
                    .args([
                        "--exact",
                        if reclamation { "core::service_daemon::high_priority::calibration::reuse::reclaim::reclaim_replay" } else { "core::service_daemon::high_priority::calibration::reuse::ab::ab_replay" },
                        "--ignored",
                        "--nocapture",
                    ])
                    .env("SD_REUSE_CASE", output.join(&name))
                    .output()?;
                fs::write(output.join(format!("{name}.replay.stdout")), &replay.stdout)?;
                fs::write(output.join(format!("{name}.replay.stderr")), &replay.stderr)?;
                Some(replay.status)
            } else {
                None
            };
            let case_success = status.success() && (!paired || replay.is_some_and(|s| s.success()));
            let row = json!({"case":name,"capacity":capacity,"affinity":affinity,"arm":arm,"success":case_success,
                "replay_exit_code":replay.and_then(|s|s.code()),
                "exit_code":status.code(),"seconds":start.elapsed().as_secs_f64()});
            eprintln!("{row}");
            save(&output.join(format!("{name}.exit.json")), &row)?;
            cases.push(row);
            success &= case_success;
        }
    }
    artifacts::verify(&output.join("sources"), &sources)?;
    ensure!(
        artifacts::hash(&fs::read(&executable)?) == binary_hash,
        "executable changed"
    );
    let mut report = if reclamation {
        String::from(
            "# Retain/reuse vs reclaim/rebuild\n\nFour pairs; same three-CPU affinity and executable; 300-second valley and 50-second second pressure phase. Test-only controlled tail reclamation, not a production scale-down controller.\n\n| Case | Exit | Outcome | Before workers | Target shard | After workers |\n|---|---:|---|---:|---:|---:|\n",
        )
    } else if comparison {
        String::from(
            "# Same-budget growth-first vs idle-first experiment\n\nFour independent pairs, alternating arm order, real 65-second valleys, capacity 3. Idle-first is a cfg(test)-only variant; production defaults are unchanged.\n\n| Case | Exit | Outcome | Before workers | Target shard | After workers |\n|---|---:|---|---:|---:|---:|\n",
        )
    } else if cycles {
        String::from(
            "# Repeated pressure cycles\n\nSix natural pressure episodes per process, real 65-second valleys, fixed inferred budget 3, production policy. This is not scale-down, a long-duration soak or a universal latency guarantee.\n\n| Case / round | Exit | Outcome | Before workers | Target shard | After workers |\n|---|---:|---|---:|---:|---:|\n",
        )
    } else {
        String::from(
            "# Empty-shard reuse experiment\n\nTwo natural pressure episodes, real 65-second valley, production policy. CPU affinity bounds capacity without policy overrides. This is not scale-down or a universal latency guarantee.\n\n| Case | Exit | Outcome | Before workers | Target shard | After workers |\n|---|---:|---|---:|---:|---:|\n",
        )
    };
    use std::fmt::Write;
    for row in &cases {
        let name = row["case"].as_str().context("missing case name")?;
        let path = output.join(name).join("summary.json");
        if path.exists() {
            let summary: Value = serde_json::from_slice(&fs::read(path)?)?;
            if cycles {
                for round in summary["rounds"].as_array().context("missing rounds")? {
                    writeln!(
                        report,
                        "| {name} / {} | {} | {} | {} | {} | {} |",
                        round["round"],
                        row["exit_code"],
                        round["outcome"],
                        round["workers_before"],
                        round["target"],
                        round["workers_after"]
                    )?;
                }
                continue;
            }
            writeln!(
                report,
                "| {name} | {} | {} | {} | {} | {} |",
                row["exit_code"],
                summary["outcome"],
                summary["valley"]["workers"],
                summary["second_target"],
                summary["workers_after"]
            )?;
        } else {
            writeln!(
                report,
                "| {name} | {} | Insufficient evidence / inspect raw failure | - | - | - |",
                row["exit_code"]
            )?;
        }
    }
    if comparison {
        report.push_str(&ab::comparison_report(&output, &cases)?);
    }
    if reclamation {
        report.push_str(&reclaim::comparison_report(&output, &cases)?);
    }
    fs::write(output.join("report.md"), report)?;
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(&output)? {
        let entry = entry?;
        let name = entry.file_name().to_str().context("bad path")?.to_owned();
        if entry.file_type()?.is_file() {
            files.insert(name, artifacts::hash(&fs::read(entry.path())?));
        } else if cases.iter().any(|c| c["case"] == name) {
            for file in fs::read_dir(entry.path())? {
                let file = file?;
                ensure!(file.file_type()?.is_file(), "unexpected case directory");
                files.insert(
                    format!("{name}/{}", file.file_name().to_str().context("bad path")?),
                    artifacts::hash(&fs::read(file.path())?),
                );
            }
        }
    }
    save(
        &output.join("manifest.json"),
        &json!({"schema":1,"sources":sources,"artifacts":files,"cases":cases,
        "binary_sha256":binary_hash,"success":success}),
    )?;
    ensure!(
        success,
        "some cases lack complete evidence; preserve artifacts"
    );
    Ok(())
}

#[test]
fn reuse_classification_requires_real_empty_target_and_resource_evidence() -> Result<()> {
    let mut valley = Valley {
        workers: 2,
        capacity: 2,
        existing: vec![0, 1],
        empty_nominal: vec![1],
    };
    assert_eq!(classify(&valley, 1, 2)?, PlacementOutcome::ReusedEmpty);
    assert!(classify(&valley, 2, 3).is_err());
    valley.capacity = 3;
    assert_eq!(
        classify(&valley, 2, 3)?,
        PlacementOutcome::AllocatedDespiteEmpty
    );
    assert!(classify(&valley, 0, 2).is_err());
    assert!(classify(&valley, 1, 3).is_err());
    valley.empty_nominal.clear();
    assert!(classify(&valley, 2, 3).is_err());
    Ok(())
}
