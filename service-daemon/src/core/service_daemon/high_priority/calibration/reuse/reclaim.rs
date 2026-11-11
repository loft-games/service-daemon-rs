//! Controlled empty-tail reclamation experiment, never a production controller.
use super::*;
use crate::core::service_daemon::high_priority::HighPriorityRuntimePool;
use crate::models::{HighPriorityShardId, HighPriorityShardPressureState, ServiceInstanceId};

fn now_ns() -> u64 {
    START.get().unwrap().elapsed().as_nanos() as u64
}

fn proc_cpu_ticks(text: &str) -> Result<u64> {
    let fields: Vec<_> = text
        .rsplit_once(") ")
        .context("bad process stat")?
        .1
        .split_whitespace()
        .collect();
    let user: u64 = fields.get(11).context("missing utime")?.parse()?;
    let system: u64 = fields.get(12).context("missing stime")?.parse()?;
    user.checked_add(system).context("CPU tick overflow")
}

#[test]
fn reclaim_cpu_parser_and_idle_rates_reject_counter_or_identity_changes() -> Result<()> {
    assert_eq!(
        proc_cpu_ticks("12 (name with ) spaces) R 1 2 3 4 5 6 7 8 9 10 17 23 0")?,
        40
    );
    assert!(proc_cpu_ticks("invalid").is_err());
    let a = json!({"threads":{"12":{"name":"svc-high-pri","start_ticks":4,"cpu_ns":100,"voluntary":3,"involuntary":1}}});
    let mut b = a.clone();
    b["threads"]["12"]["cpu_ns"] = json!(1_000_100);
    b["threads"]["12"]["voluntary"] = json!(5);
    let rates = thread_rates(&a, &b, 2.0)?;
    assert!(thread_rates(&a, &b, f64::NAN).is_err());
    assert_eq!(rates["cpu_ms_per_second"], 0.5);
    assert_eq!(rates["context_switches_per_second"], 1.0);
    b["threads"]["12"]["start_ticks"] = json!(5);
    assert!(thread_rates(&a, &b, 2.0).is_err());
    b = a.clone();
    b["threads"]["12"]["cpu_ns"] = json!(0);
    assert!(thread_rates(&a, &b, 2.0).is_err());
    b["threads"] = json!({});
    assert!(thread_rates(&a, &b, 2.0).is_err());
    Ok(())
}

pub(super) fn os_sample() -> Result<Value> {
    let mut value = super::super::idle::process_sample_json()?;
    value["process_cpu_ticks"] = json!(proc_cpu_ticks(&fs::read_to_string("/proc/self/stat")?)?);
    value["at_ns"] = json!(now_ns());
    Ok(value)
}

async fn until(daemon: &DaemonInstanceHandle, deadline: u64) {
    while now_ns() < deadline {
        capture(daemon).await;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

pub(super) async fn long_valley(
    daemon: &DaemonInstanceHandle,
    remove: bool,
    valley_start: u64,
) -> Result<Value> {
    let initial_tid = shard_tid(daemon, HighPriorityShardId(0)).await?;
    let target_tid = shard_tid(daemon, HighPriorityShardId(1)).await?;
    // An extra guard interval leaves at least 65 seconds of sampled idle eligibility.
    until(daemon, valley_start + 75_000_000_000).await;
    let before = os_sample()?;
    let action_at = now_ns();
    let receipt = if remove {
        reclaim_tail(daemon, HighPriorityShardId(1)).await?
    } else {
        json!({"retained":true})
    };
    let after = os_sample()?;
    ensure!(
        Path::new(&format!("/proc/self/task/{initial_tid}")).exists(),
        "initial worker lost"
    );
    ensure!(
        Path::new(&format!("/proc/self/task/{target_tid}")).exists() != remove,
        "target worker lifetime mismatch"
    );
    capture(daemon).await;
    // Exclude transient helper/blocking-pool threads from the stable idle window.
    until(daemon, valley_start + 95_000_000_000).await;
    let idle_before = os_sample()?;
    until(daemon, valley_start + 295_000_000_000).await;
    let idle_after = os_sample()?;
    until(daemon, valley_start + 300_000_000_000).await;
    let before_pressure = os_sample()?;
    let hz = std::process::Command::new("getconf")
        .arg("CLK_TCK")
        .output()?;
    ensure!(hz.status.success(), "CLK_TCK unavailable");
    let hz: u64 = String::from_utf8(hz.stdout)?.trim().parse()?;
    Ok(
        json!({"remove":remove,"action_at_ns":action_at,"valley_seconds":300,
        "initial_tid":initial_tid,"target_tid":target_tid,"clock_ticks_per_second":hz,
        "before":before,"after":after,"idle_before":idle_before,"idle_after":idle_after,
        "before_pressure":before_pressure,"receipt":receipt}),
    )
}

pub(super) fn metrics(records: &[Value], checkpoints: &[Value]) -> Result<Value> {
    ensure!(checkpoints.len() == 1, "missing/ambiguous checkpoint");
    let c = &checkpoints[0];
    let data = &c["reclamation"];
    let remove = data["remove"].as_bool().context("missing arm")?;
    ensure!(
        records[0]["data"]["reclaim"] == remove && records[0]["data"]["available_parallelism"] == 3,
        "arm/budget mismatch"
    );
    ensure!(data["valley_seconds"] == 300, "valley duration changed");
    let second = c["second"].as_str().context("missing subject")?;
    let onset = c["second_pressure_at_ns"]
        .as_u64()
        .context("missing onset")?;
    let first_effect = c["first_effect_at_ns"]
        .as_u64()
        .context("missing first effect")?;
    ensure!(onset - first_effect >= 300_000_000_000, "short valley");
    let action = data["action_at_ns"].as_u64().context("missing action")?;
    let eligible: Vec<_> = records
        .iter()
        .filter(|r| {
            r["kind"] == "resources"
                && r["at_ns"]
                    .as_u64()
                    .is_some_and(|t| t >= first_effect + 5_000_000_000 && t < action)
        })
        .collect();
    ensure!(
        eligible.len() > 250
            && eligible.last().unwrap()["at_ns"].as_u64().unwrap()
                - eligible[0]["at_ns"].as_u64().unwrap()
                >= 65_000_000_000,
        "insufficient sustained idle samples"
    );
    for r in &eligible {
        ensure!(
            r["data"]["shards"]
                .as_array()
                .context("missing shards")?
                .iter()
                .any(|s| s["id"] == 1
                    && s["active"] == 0
                    && s["assigned"] == 0
                    && s["pressure"] == "Nominal"),
            "target not continuously empty nominal"
        );
    }
    for pair in eligible.windows(2) {
        ensure!(
            pair[1]["at_ns"].as_u64().unwrap() - pair[0]["at_ns"].as_u64().unwrap() < 1_000_000_000,
            "idle evidence gap"
        );
    }
    let tid = data["target_tid"]
        .as_u64()
        .context("missing TID")?
        .to_string();
    let initial = data["initial_tid"]
        .as_u64()
        .context("missing initial TID")?
        .to_string();
    ensure!(
        !data["before"]["threads"][&tid].is_null(),
        "worker never existed"
    );
    for name in ["after", "idle_before", "idle_after", "before_pressure"] {
        ensure!(
            !data[name]["threads"][&initial].is_null(),
            "initial worker disappeared"
        );
        ensure!(
            data[name]["threads"][&tid].is_null() == remove,
            "OS worker release mismatch"
        );
    }
    if remove {
        ensure!(
            data["receipt"]["probe_joined"] == true
                && data["receipt"]["runtime_joined"] == true
                && data["receipt"]["workers_after"] == 1,
            "incomplete shutdown"
        );
    }
    let gens: Vec<_> = records
        .iter()
        .filter(|r| {
            r["kind"] == "generation"
                && r["data"]["instance"] == second
                && r["data"]["generation"] == 2
        })
        .collect();
    ensure!(
        gens.len() == 1 && gens[0]["data"]["shard"] == if remove { 2 } else { 1 },
        "wrong actual placement"
    );
    let gen_at = gens[0]["at_ns"]
        .as_u64()
        .context("missing generation time")?;
    let effects: Vec<_> = records
        .iter()
        .filter(|r| {
            r["data"]["message"] == "HighPriority intervention evaluated"
                && r["data"]["service_instance_id"] == second
        })
        .collect();
    ensure!(effects.len() == 1, "effect count mismatch");
    let effect_at = effects[0]["at_ns"]
        .as_u64()
        .context("missing effect time")?;
    ensure!(onset < gen_at && gen_at < effect_at, "invalid timing");
    let end = onset + 50_000_000_000;
    let post_start = gen_at + 2_000_000_000;
    let post_end = post_start + 30_000_000_000;
    ensure!(
        post_end <= end
            && records
                .iter()
                .any(|r| r["kind"] == "measurement_end"
                    && r["at_ns"].as_u64().is_some_and(|t| t >= end)),
        "incomplete fixed windows"
    );
    let creates: Vec<_> = records
        .iter()
        .filter(|r| {
            r["data"]["message"] == "HighPriority experimental shard creation measured"
                && r["at_ns"].as_u64().is_some_and(|t| t > onset)
        })
        .collect();
    ensure!(
        creates.len() == usize::from(remove),
        "unexpected rebuild count"
    );
    if remove {
        ensure!(
            creates[0]["data"]["shard"] == 2,
            "reused retired shard identity"
        );
    }
    let a = &data["idle_before"];
    let b = &data["idle_after"];
    let seconds = (b["at_ns"].as_u64().unwrap() - a["at_ns"].as_u64().unwrap()) as f64 / 1e9;
    ensure!((199.0..202.0).contains(&seconds), "idle window changed");
    let hz = data["clock_ticks_per_second"]
        .as_u64()
        .context("missing tick rate")?;
    ensure!(hz > 0, "bad tick rate");
    let cpu_ticks = b["process_cpu_ticks"]
        .as_u64()
        .unwrap()
        .checked_sub(a["process_cpu_ticks"].as_u64().unwrap())
        .context("CPU reset")?;
    let thread_rates = thread_rates(a, b, seconds)?;
    let phase_os: Vec<_> = records
        .iter()
        .filter(|r| {
            r["kind"] == "event"
                && r["data"]["message"] == "HighPriority experimental OS second phase ended"
        })
        .collect();
    ensure!(phase_os.len() == 1, "missing second phase OS snapshot");
    Ok(
        json!({"remove":remove,"resources":data,"idle_seconds":seconds,"idle_thread_rates":thread_rates,
        "os_second_phase_end":phase_os[0]["data"]["os"],
        "idle_process_cpu_ms_per_second":cpu_ticks as f64*1000.0/hz as f64/seconds,
        "generation_delay_ns":gen_at-onset,"evaluation_delay_ns":effect_at-onset,
        "creation_ns":creates.first().map(|r|r["data"]["creation_ns"].clone()),
        "phase":ab::window(records,second,None,onset,end)?,
        "post_generation":ab::window(records,second,Some(2),post_start,post_end)?}),
    )
}

fn thread_rates(a: &Value, b: &Value, seconds: f64) -> Result<Value> {
    ensure!(
        seconds.is_finite() && seconds > 0.0,
        "invalid idle elapsed time"
    );
    let before = a["threads"].as_object().context("missing threads")?;
    let after = b["threads"].as_object().context("missing threads")?;
    ensure!(
        before.keys().eq(after.keys()),
        "stable idle thread set changed"
    );
    let mut cpu = 0u64;
    let mut switches = 0u64;
    let mut hp_cpu = 0u64;
    for (tid, old) in before {
        let new = &after[tid];
        ensure!(
            old["start_ticks"] == new["start_ticks"] && old["name"] == new["name"],
            "TID reused"
        );
        let delta = new["cpu_ns"]
            .as_u64()
            .context("missing CPU")?
            .checked_sub(old["cpu_ns"].as_u64().context("missing CPU")?)
            .context("CPU reset")?;
        cpu += delta;
        if old["name"]
            .as_str()
            .is_some_and(|n| n.starts_with("svc-high-pri"))
        {
            hp_cpu += delta;
        }
        for key in ["voluntary", "involuntary"] {
            switches += new[key]
                .as_u64()
                .context("missing switch count")?
                .checked_sub(old[key].as_u64().context("missing switch count")?)
                .context("switch reset")?;
        }
    }
    Ok(
        json!({"cpu_ms_per_second":cpu as f64/1e6/seconds,"hp_cpu_ms_per_second":hp_cpu as f64/1e6/seconds,"context_switches_per_second":switches as f64/seconds}),
    )
}

#[test]
#[ignore = "controlled long-valley case; three CPU affinity, SD_REUSE_CASE and SD_PLACEMENT_ARM"]
fn reclaim_case() -> Result<()> {
    let remove = match std::env::var("SD_PLACEMENT_ARM")?.as_str() {
        "retain" => false,
        "reclaim" => true,
        _ => anyhow::bail!("bad arm"),
    };
    let output = std::path::PathBuf::from(std::env::var("SD_REUSE_CASE")?);
    fs::create_dir(&output)?;
    START.set(Instant::now()).unwrap();
    tracing::subscriber::set_global_default(TraceRegistry::default().with(Trace))?;
    record(
        "case",
        json!({"schema":1,"mode":"adaptive","scenario":"contention","seconds":90,"blocking_work_ms":400,
        "build":if cfg!(debug_assertions){"debug"}else{"release"},"available_parallelism":std::thread::available_parallelism()?.get(),"reclaim":remove}),
    );
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?
        .block_on(run_case_variant(
            &output,
            Some(ab::Arm::IdleFirst),
            Some(remove),
        ))
}

fn rebuild(records: &[Value], c: &[Value]) -> Result<Value> {
    ensure!(c.len() == 1, "bad checkpoint count");
    let checkpoint = &c[0];
    let valley: Valley = serde_json::from_value(checkpoint["valley"].clone())?;
    ensure!(
        valley.capacity == 3
            && valley.workers == 2
            && valley.existing == [0, 1]
            && valley.empty_nominal == [1],
        "bad initial topology"
    );
    let remove = checkpoint["reclamation"]["remove"]
        .as_bool()
        .context("missing arm")?;
    let mut summary = validate_case_topology(
        records,
        checkpoint["first"].as_str().context("missing first")?,
        checkpoint["second"].as_str().context("missing second")?,
        checkpoint["competitor"]
            .as_str()
            .context("missing competitor")?,
        &valley,
        2,
        remove,
    )?;
    summary["reclamation"] = metrics(records, c)?;
    Ok(summary)
}

#[test]
#[ignore = "complete read-only reclaim replay; SD_REUSE_CASE"]
fn reclaim_replay() -> Result<()> {
    let p = std::path::PathBuf::from(std::env::var("SD_REUSE_CASE")?);
    let raw: Vec<Value> = serde_json::from_slice(&fs::read(p.join("raw.json"))?)?;
    let c: Vec<Value> = serde_json::from_slice(&fs::read(p.join("checkpoints.json"))?)?;
    ensure!(
        serde_json::to_vec_pretty(&rebuild(&raw, &c)?)? == fs::read(p.join("summary.json"))?,
        "summary byte mismatch"
    );
    let mut wrong = c.clone();
    wrong[0]["reclamation"]["remove"] = json!(!c[0]["reclamation"]["remove"].as_bool().unwrap());
    ensure!(rebuild(&raw, &wrong).is_err(), "wrong arm accepted");
    let missing: Vec<_> = raw
        .iter()
        .filter(|r| {
            !(r["kind"] == "generation"
                && r["data"]["instance"] == c[0]["second"]
                && r["data"]["generation"] == 2)
        })
        .cloned()
        .collect();
    ensure!(
        rebuild(&missing, &c).is_err(),
        "missing generation accepted"
    );
    let mut missing = c.clone();
    missing[0]["reclamation"]["receipt"] = json!({});
    if c[0]["reclamation"]["remove"] == true {
        ensure!(
            rebuild(&raw, &missing).is_err(),
            "missing runtime/probe completion accepted"
        );
    }
    Ok(())
}

pub(super) fn comparison_report(root: &Path, cases: &[Value]) -> Result<String> {
    use std::fmt::Write;
    let mut text = String::from(
        "\n## Resource and recovery measurements\n\nIdle rates cover the stable ~200-second window. Memory values are process endpoints, not per-shard allocations. Post tails use exactly 30 seconds after 2-second generation settling; phase tails cover the full second 50-second pressure phase. P99.9 below 10,000 samples is exploratory.\n\n| Case | Idle threads | FDs | RSS KiB | PSS KiB | VmSize KiB | CPU ms/s (schedstat) | HP CPU ms/s | CS/s | Generation ms | Evaluation ms | Creation ms | Post P99 ms | Post P99.9 ms | Post Max ms | Phase P99.9 ms |\n|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n",
    );
    for c in cases {
        let name = c["case"].as_str().context("bad case")?;
        if c["success"] != true {
            writeln!(text, "| {name} | Insufficient evidence; inspect failure |")?;
            continue;
        }
        let s: Value = serde_json::from_slice(&fs::read(root.join(name).join("summary.json"))?)?;
        let m = &s["reclamation"];
        let os = &m["resources"]["idle_after"];
        let rates = &m["idle_thread_rates"];
        let ms = |v: &Value| v.as_u64().unwrap_or(0) as f64 / 1e6;
        let creation = m["creation_ns"]
            .as_u64()
            .map_or_else(|| "n/a".into(), |n| format!("{:.6}", n as f64 / 1e6));
        writeln!(
            text,
            "| {name} | {} | {} | {} | {} | {} | {:.6} | {:.6} | {:.3} | {:.3} | {:.3} | {} | {:.6} | {:.6} | {:.6} | {:.6} |",
            os["threads"].as_object().unwrap().len(),
            os["file_descriptors"],
            os["rss_kib"],
            os["pss_kib"],
            os["virtual_kib"],
            rates["cpu_ms_per_second"].as_f64().unwrap(),
            rates["hp_cpu_ms_per_second"].as_f64().unwrap(),
            rates["context_switches_per_second"].as_f64().unwrap(),
            ms(&m["generation_delay_ns"]),
            ms(&m["evaluation_delay_ns"]),
            creation,
            ms(&m["post_generation"]["p99_ns"]),
            ms(&m["post_generation"]["p999_ns"]),
            ms(&m["post_generation"]["max_ns"]),
            ms(&m["phase"]["p999_ns"])
        )?;
    }
    Ok(text)
}

fn detach_empty_tail(
    pool: &mut HighPriorityRuntimePool,
    id: HighPriorityShardId,
) -> Result<(tokio::runtime::Runtime, usize)> {
    let mut shards = pool.state.shards.write().unwrap();
    ensure!(
        id.0 != 0 && shards.len() > 1 && shards.last().is_some_and(|s| s.shard_id == id),
        "only the dynamic tail is eligible"
    );
    ensure!(
        shards.len() == pool.runtimes.len(),
        "runtime/handle ownership mismatch"
    );
    ensure!(
        !pool.state.assignments.iter().any(|e| *e.value() == id)
            && !pool
                .state
                .active_generations
                .iter()
                .any(|e| *e.value() == id)
            && !pool
                .state
                .pending_rollovers
                .iter()
                .any(|e| *e.value() == id)
            && !pool.state.next_placements.iter().any(|e| e.value().1 == id),
        "shard has live or reserved ownership"
    );
    ensure!(
        pool.state
            .inner
            .pressure_state
            .get(&id)
            .is_some_and(|p| *p == HighPriorityShardPressureState::Nominal),
        "shard is not nominal"
    );
    let handle = shards.pop().context("missing handle")?;
    pool.state.inner.pressure_state.remove(&id);
    // No concurrent service creation in this controlled experiment. Holding the
    // shard write lock excludes select_generation while detaching this target.
    Ok((
        pool.runtimes.pop().context("missing runtime")?,
        handle.worker_threads,
    ))
}

async fn reclaim_tail(daemon: &DaemonInstanceHandle, id: HighPriorityShardId) -> Result<Value> {
    let start = Instant::now();
    let mut inner = daemon.inner.lock().await;
    let (task_id, token) = inner
        .high_priority_runtime_pool
        .experiment_probes
        .get(&id)
        .context("missing target probe")?
        .clone();
    let index = inner
        .runtime_probe_tasks
        .iter()
        .position(|t| t.id() == task_id)
        .context("probe task not owned")?;
    let (runtime, workers) = detach_empty_tail(&mut inner.high_priority_runtime_pool, id)?;
    // Keep the runtime owned by a helper until its probe is joined, even on error.
    // The helper's thread CPU/stack cost is included in process measurements.
    let (close, wait) = std::sync::mpsc::channel();
    let closer = std::thread::spawn(move || {
        let _ = wait.recv();
        drop(runtime);
    });
    token.cancel();
    let probe = inner.runtime_probe_tasks.remove(index);
    tokio::time::timeout(Duration::from_secs(5), probe).await??;
    let probe_ns = start.elapsed().as_nanos() as u64;
    close.send(()).context("runtime closer ended prematurely")?;
    tokio::task::spawn_blocking(move || {
        closer
            .join()
            .map_err(|_| anyhow::anyhow!("runtime closer panicked"))
    })
    .await??;
    inner.high_priority_runtime_pool.total_worker_threads = inner
        .high_priority_runtime_pool
        .total_worker_threads
        .checked_sub(workers)
        .context("budget underflow")?;
    inner
        .high_priority_runtime_pool
        .experiment_probes
        .remove(&id);
    inner
        .resources
        .runtime_facts
        .record_high_priority_shards(inner.high_priority_runtime_pool.snapshot());
    Ok(
        json!({"shard":id.0,"probe_join_ns":probe_ns,"shutdown_ns":start.elapsed().as_nanos() as u64,
        "probe_joined":true,"runtime_joined":true,"workers_after":inner.high_priority_runtime_pool.total_worker_threads()}),
    )
}

async fn shard_tid(daemon: &DaemonInstanceHandle, id: HighPriorityShardId) -> Result<u32> {
    let handle = daemon
        .inner
        .lock()
        .await
        .high_priority_runtime_pool
        .shard_handle(id)
        .context("missing shard")?
        .handle;
    handle
        .spawn(async {
            fs::read_link("/proc/thread-self")?
                .file_name()
                .context("missing TID")?
                .to_str()
                .context("invalid TID")?
                .parse()
                .context("bad TID")
        })
        .await?
}

#[tokio::test]
async fn reclaim_joins_only_target_probe_and_worker() -> Result<()> {
    let daemon = ServiceDaemon::builder()
        .with_registry(Registry::builder().with_tag("__reuse_experiment__").build())
        .build();
    daemon.run().await;
    let result = async {
        {
            let mut inner = daemon.inner.lock().await;
            inner.high_priority_runtime_pool.create_shard(1)?;
            let pool = &mut inner.high_priority_runtime_pool;
            let control = tokio::runtime::Handle::current();
            let shard = pool.shard_handle(HighPriorityShardId(1)).unwrap().handle;
            let token = inner.cancellation_token.child_token();
            let task = control.spawn(
                crate::core::diagnostics::run_high_priority_shard_runtime_probe(
                    inner.diagnostics.clone(),
                    HighPriorityShardId(1),
                    shard,
                    token.clone(),
                ),
            );
            inner
                .high_priority_runtime_pool
                .experiment_probes
                .insert(HighPriorityShardId(1), (task.id(), token));
            inner.runtime_probe_tasks.push(task);
            inner.high_priority_runtime_pool.state.record_pressure(
                HighPriorityShardId(1),
                HighPriorityShardPressureState::Nominal,
            );
        }
        let original = shard_tid(&daemon, HighPriorityShardId(0)).await?;
        let retired = shard_tid(&daemon, HighPriorityShardId(1)).await?;
        let before = daemon.inner.lock().await.runtime_probe_tasks.len();
        let receipt = reclaim_tail(&daemon, HighPriorityShardId(1)).await?;
        ensure!(receipt["workers_after"] == 1, "budget not released");
        ensure!(
            !Path::new(&format!("/proc/self/task/{retired}")).exists(),
            "retired worker still alive"
        );
        ensure!(
            Path::new(&format!("/proc/self/task/{original}")).exists(),
            "initial worker disappeared"
        );
        let inner = daemon.inner.lock().await;
        ensure!(
            inner.runtime_probe_tasks.len() + 1 == before
                && inner.runtime_probe_tasks.iter().all(|t| !t.is_finished()),
            "unrelated probe stopped"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    daemon.shutdown();
    tokio::time::timeout(Duration::from_secs(10), daemon.wait()).await??;
    result
}

#[test]
fn reclaim_rejects_busy_reserved_initial_and_rebuilds_with_new_identity() -> Result<()> {
    use crate::core::service_daemon::runtime::HighPriorityCapacityPlan;
    use crate::models::policy::HighPriorityRuntimeControl;
    use std::num::NonZeroUsize;
    let mut pool = HighPriorityRuntimePool::new_with_parallelism(
        HighPriorityRuntimeControl::for_testing(),
        HighPriorityCapacityPlan::from_entry_count(1, NonZeroUsize::new(3)),
        NonZeroUsize::new(3),
    );
    pool.prepare_initial_runtime()?;
    pool.scale_out(Instant::now())?;
    let id = HighPriorityShardId(1);
    assert!(detach_empty_tail(&mut pool, HighPriorityShardId(0)).is_err());
    assert!(detach_empty_tail(&mut pool, id).is_err()); // Unknown
    pool.state
        .record_pressure(id, HighPriorityShardPressureState::Nominal);
    pool.state
        .record_pressure(id, HighPriorityShardPressureState::Pressured);
    assert!(detach_empty_tail(&mut pool, id).is_err());
    pool.state
        .record_pressure(id, HighPriorityShardPressureState::Nominal);
    let instance = ServiceInstanceId::new(uuid::Uuid::from_u128(0xface));
    pool.state.active_generations.insert((instance, 1), id);
    assert!(detach_empty_tail(&mut pool, id).is_err());
    pool.state.active_generations.clear();
    pool.state.assignments.insert(instance, id);
    assert!(detach_empty_tail(&mut pool, id).is_err());
    pool.state.assignments.clear();
    pool.state.pending_rollovers.insert((instance, 1), id);
    assert!(detach_empty_tail(&mut pool, id).is_err());
    pool.state.pending_rollovers.clear();
    pool.state.next_placements.insert(instance, (1, id));
    assert!(detach_empty_tail(&mut pool, id).is_err());
    pool.state.next_placements.clear();
    let (runtime, workers) = detach_empty_tail(&mut pool, id)?;
    assert_eq!(workers, 1);
    assert_eq!(pool.total_worker_threads(), 2); // Not credited before shutdown.
    assert!(pool.shard_handle(id).is_none());
    assert_eq!(
        pool.state
            .select_generation(instance, 2)
            .unwrap()
            .0
            .shard_id,
        HighPriorityShardId(0)
    );
    drop(runtime);
    pool.total_worker_threads -= workers;
    let new = pool
        .scale_out(Instant::now() + Duration::from_secs(60))?
        .unwrap();
    assert_eq!(new, HighPriorityShardId(2));
    assert_eq!(pool.total_worker_threads(), 2);
    pool.create_shard(1)?;
    pool.state
        .record_pressure(new, HighPriorityShardPressureState::Nominal);
    assert!(detach_empty_tail(&mut pool, new).is_err()); // Non-tail dynamic shard.
    pool.shutdown();
    Ok(())
}
