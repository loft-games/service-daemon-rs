//! Linux-only, opt-in idle resource cost experiment; no production policy changes.
use crate::core::diagnostics::{RuntimeLane, run_high_priority_shard_runtime_probe};
use crate::core::service_daemon::DaemonInstanceHandle;
use crate::{Registry, ServiceDaemon, service};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

const CASE: &str = "core::service_daemon::high_priority::calibration::idle::idle_cost_case";

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    serde_json::to_writer_pretty(
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?,
        value,
    )?;
    Ok(())
}

#[test]
#[ignore = "Linux release idle-cost experiment; fresh SD_IDLE_DIR required, about 10 minutes"]
fn idle_cost_experiment() -> Result<()> {
    use super::artifacts;
    let output = std::path::PathBuf::from(std::env::var("SD_IDLE_DIR")?);
    ensure!(output.is_absolute(), "SD_IDLE_DIR must be absolute");
    let seconds: u64 = std::env::var("SD_IDLE_SECONDS")
        .unwrap_or_else(|_| "30".into())
        .parse()?;
    ensure!((1..=60).contains(&seconds), "invalid idle window");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("missing root")?;
    fs::create_dir(&output)?;
    let sources = artifacts::snapshot(
        root,
        &output.join("sources"),
        &artifacts::source_paths(root)?,
    )?;
    let version = |name: &str| -> Result<String> {
        let out = Command::new(name).arg("--version").output()?;
        ensure!(out.status.success(), "tool version failed");
        Ok(String::from_utf8(out.stdout)?.trim().into())
    };
    write_json(
        &output.join("inputs.json"),
        &serde_json::json!({"schema":1,"sources":sources,
        "rustc":version("rustc")?,"cargo":version("cargo")?, "seconds":seconds,
        "repetitions":3,"profile":"release","kernel":fs::read_to_string("/proc/sys/kernel/osrelease")?,
        "available_parallelism":std::thread::available_parallelism()?.get(),
        "preflight_loadavg":fs::read_to_string("/proc/loadavg")?}),
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
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
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
        .context("test executable missing")?;
    let executable = output.join("idle-test");
    fs::copy(binary, &executable)?;
    let binary_hash = artifacts::hash(&fs::read(&executable)?);
    let mut success = true;
    for repetition in 1..=3 {
        eprintln!("idle-cost repetition {repetition}/3, {seconds}s windows");
        let mut child = Command::new(&executable)
            .args(["--exact", CASE, "--ignored", "--nocapture"])
            .env("SD_IDLE_SECONDS", seconds.to_string())
            .env("SD_IDLE_REPETITION", repetition.to_string())
            .env(
                "SD_IDLE_OUTPUT",
                output.join(format!("case-{repetition}.json")),
            )
            .stdout(Stdio::from(fs::File::create(
                output.join(format!("case-{repetition}.stdout")),
            )?))
            .stderr(Stdio::from(fs::File::create(
                output.join(format!("case-{repetition}.stderr")),
            )?))
            .spawn()?;
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if start.elapsed() > Duration::from_secs(6 * seconds + 120) {
                child.kill()?;
                break child.wait()?;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        write_json(
            &output.join(format!("case-{repetition}.exit.json")),
            &serde_json::json!({"success":status.success(),"code":status.code(),"elapsed_seconds":start.elapsed().as_secs_f64()}),
        )?;
        success &= status.success();
    }
    artifacts::verify(&output.join("sources"), &sources)?;
    ensure!(
        artifacts::hash(&fs::read(&executable)?) == binary_hash,
        "executable changed"
    );
    if success {
        render_report(&output, seconds)?;
    }
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(&output)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            files.insert(
                entry
                    .file_name()
                    .to_str()
                    .context("invalid artifact name")?
                    .to_owned(),
                artifacts::hash(&fs::read(entry.path())?),
            );
        }
    }
    write_json(
        &output.join("manifest.json"),
        &serde_json::json!({"schema":1,"sources":sources,
        "binary_sha256":binary_hash,"artifacts":files,"success":success}),
    )?;
    ensure!(
        success,
        "one or more idle cases failed; retain raw artifacts"
    );
    Ok(())
}

fn render_report(output: &Path, seconds: u64) -> Result<()> {
    use std::fmt::Write;
    let mut text = format!(
        "# Linux idle resource cost\n\nWindow: {seconds}s. Three independent processes; HP probe order alternates.\n\nNo production policy changes. Topology is constructed through the private shard constructor, not automatic pressure-driven scaling. Services are started on every shard and removed before measurement. Control/Standard probes and policy remain running while HP probes are toggled.\n\nCPU is summed per-thread schedstat runtime, not wall time. 10 ms/s = 1% of one logical CPU. Context switches include voluntary and involuntary counts. RSS/PSS are process-wide smaps_rollup endpoint values; VmSize is virtual address space, not resident cost. Sampler overhead is included in process CPU but not HP worker CPU.\n\n| Rep | Phase | HP workers | HP probes | Threads | RSS KiB | PSS KiB | Private KiB | VmSize KiB | FDs | Process CPU ms/s | HP CPU ms/s | Process CS/s | HP CS/s |\n|---|---|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n"
    );
    for repetition in 1..=3 {
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(output.join(format!("case-{repetition}.json")))?)?;
        let windows: Vec<Window> = serde_json::from_value(value["windows"].clone())?;
        for w in windows {
            writeln!(
                text,
                "| {repetition} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {:.4} | {:.4} | {:.3} | {:.3} |",
                w.phase,
                w.expected_hp_workers,
                w.hp_probes_enabled,
                w.after.threads.len(),
                w.after.rss_kib,
                w.after.pss_kib,
                w.after.private_kib,
                w.after.virtual_kib,
                w.after.file_descriptors,
                w.process.cpu_ms_per_second,
                w.high_priority.cpu_ms_per_second,
                w.process.voluntary_per_second + w.process.involuntary_per_second,
                w.high_priority.voluntary_per_second + w.high_priority.involuntary_per_second
            )?;
        }
    }
    text.push_str("\nShort windows are smoke-only, not cost evidence. No universal low-cost threshold is asserted. Before/after-daemon rows include control runtime and other daemon ownership differences; they are not single-shard reclamation results. Endpoint sleep states do not prove uninterrupted sleep. Thread identity and monotonic counters are checked within every window.\n");
    fs::write(output.join("report.md"), text)?;
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct ProcessSample {
    threads: BTreeMap<u32, ThreadSample>,
    rss_kib: u64,
    pss_kib: u64,
    private_kib: u64,
    virtual_kib: u64,
    file_descriptors: usize,
    host_loadavg: String,
    host_memory_pressure: String,
    host_cpu_pressure: String,
    host_available_kib: u64,
}

fn process_sample() -> Result<ProcessSample> {
    let mut threads = BTreeMap::new();
    for entry in fs::read_dir("/proc/self/task")? {
        let entry = entry?;
        let path = entry.path();
        threads.insert(
            entry.file_name().to_str().context("invalid tid")?.parse()?,
            thread_sample(
                &fs::read_to_string(path.join("status"))?,
                &fs::read_to_string(path.join("stat"))?,
                &fs::read_to_string(path.join("schedstat"))?,
                &fs::read_to_string(path.join("wchan"))?,
            )?,
        );
    }
    let memory = fs::read_to_string("/proc/self/smaps_rollup")?;
    Ok(ProcessSample {
        threads,
        rss_kib: proc_number(&memory, "Rss:")?,
        pss_kib: proc_number(&memory, "Pss:")?,
        private_kib: proc_number(&memory, "Private_Clean:")?
            + proc_number(&memory, "Private_Dirty:")?,
        virtual_kib: proc_number(&fs::read_to_string("/proc/self/status")?, "VmSize:")?,
        file_descriptors: fs::read_dir("/proc/self/fd")?
            .collect::<std::io::Result<Vec<_>>>()?
            .len(),
        host_loadavg: fs::read_to_string("/proc/loadavg")?,
        host_memory_pressure: fs::read_to_string("/proc/pressure/memory")?,
        host_cpu_pressure: fs::read_to_string("/proc/pressure/cpu")?,
        host_available_kib: proc_number(&fs::read_to_string("/proc/meminfo")?, "MemAvailable:")?,
    })
}

#[derive(Debug, Serialize, Deserialize)]
struct Rates {
    cpu_ms_per_second: f64,
    voluntary_per_second: f64,
    involuntary_per_second: f64,
}

pub(super) fn process_sample_json() -> Result<serde_json::Value> {
    Ok(serde_json::to_value(process_sample()?)?)
}

fn rates(
    before: &BTreeMap<u32, ThreadSample>,
    after: &BTreeMap<u32, ThreadSample>,
    seconds: f64,
) -> Result<Rates> {
    ensure!(seconds.is_finite() && seconds > 0.0, "invalid elapsed time");
    let (cpu, voluntary, involuntary) = counter_delta(before, after)?;
    Ok(Rates {
        cpu_ms_per_second: cpu as f64 / 1e6 / seconds,
        voluntary_per_second: voluntary as f64 / seconds,
        involuntary_per_second: involuntary as f64 / seconds,
    })
}

fn hp_threads(sample: &ProcessSample) -> BTreeMap<u32, ThreadSample> {
    sample
        .threads
        .iter()
        .filter(|(_, t)| t.name.starts_with("svc-high-pri"))
        .map(|(id, t)| (*id, t.clone()))
        .collect()
}

#[derive(Debug, Serialize, Deserialize)]
struct Window {
    phase: String,
    expected_hp_workers: usize,
    hp_probes_enabled: bool,
    seconds: f64,
    before: ProcessSample,
    after: ProcessSample,
    process: Rates,
    high_priority: Rates,
    pool_before: Option<PoolEvidence>,
    pool_after: Option<PoolEvidence>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PoolEvidence {
    instances: usize,
    workers: usize,
    shards: usize,
    active: usize,
    assigned: usize,
    pending: usize,
    completed_probes: u64,
}

async fn pool_evidence(daemon: &DaemonInstanceHandle) -> PoolEvidence {
    let inner = daemon.inner.lock().await;
    let pool = &inner.high_priority_runtime_pool;
    let snapshot = pool.snapshot();
    PoolEvidence {
        instances: daemon.service_instances().len(),
        workers: pool.total_worker_threads(),
        shards: snapshot.len(),
        active: snapshot.iter().map(|s| s.active_generations).sum(),
        assigned: snapshot.iter().map(|s| s.assigned_instances).sum(),
        pending: pool.state().next_placements.len() + pool.state().pending_rollovers.len(),
        completed_probes: inner
            .diagnostics
            .snapshot()
            .high_priority_shards
            .iter()
            .map(|s| s.aggregate.runtime_probe.completed)
            .sum(),
    }
}

async fn measure(phase: &str, workers: usize, probes: bool, seconds: u64) -> Result<Window> {
    tokio::time::sleep(Duration::from_secs(3)).await;
    let start = Instant::now();
    let before = process_sample()?;
    tokio::time::sleep(Duration::from_secs(seconds)).await;
    let after = process_sample()?;
    let seconds = start.elapsed().as_secs_f64();
    let old_hp = hp_threads(&before);
    let new_hp = hp_threads(&after);
    ensure!(
        old_hp.len() == workers && new_hp.len() == workers,
        "actual HP thread count differs from pool"
    );
    let process = rates(&before.threads, &after.threads, seconds)?;
    let high_priority = rates(&old_hp, &new_hp, seconds)?;
    Ok(Window {
        phase: phase.into(),
        expected_hp_workers: workers,
        hp_probes_enabled: probes,
        seconds,
        before,
        after,
        process,
        high_priority,
        pool_before: None,
        pool_after: None,
    })
}

#[service(tags = ["__idle_cost__"], scheduling = HighPriority)]
async fn idle_cost_worker(#[input] _input: &u64) -> anyhow::Result<()> {
    crate::done();
    while crate::sleep(Duration::from_millis(10)).await {}
    Ok(())
}

async fn warm_and_remove(daemon: &DaemonInstanceHandle, count: usize) -> Result<()> {
    let resources = daemon.inner.lock().await.resources.clone();
    let handle = crate::core::context::__run_daemon_resources_sync_scope(resources, || {
        crate::service_handle!(idle_cost_worker)
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let mut instances = Vec::new();
    for _ in 0..count {
        instances.push(handle.start(0u64).await?);
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = daemon
                .inner
                .lock()
                .await
                .high_priority_runtime_pool
                .snapshot();
            if snapshot.len() == count && snapshot.iter().all(|s| s.active_generations == 1) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let warmed = hp_threads(&process_sample()?);
    ensure!(
        warmed.len() == count && warmed.values().all(|t| t.cpu_ns > 0),
        "worker schedstat CPU accounting unavailable or topology incomplete"
    );
    for instance in instances {
        ensure!(
            tokio::time::timeout(Duration::from_secs(10), instance.remove()).await??,
            "service not removed"
        );
    }
    assert_empty(daemon, count).await
}

async fn assert_empty(daemon: &DaemonInstanceHandle, count: usize) -> Result<()> {
    ensure!(daemon.service_instances().is_empty(), "instances remain");
    let inner = daemon.inner.lock().await;
    let pool = &inner.high_priority_runtime_pool;
    ensure!(
        pool.total_worker_threads() == count,
        "pool capacity changed"
    );
    ensure!(
        pool.snapshot().len() == count
            && pool
                .snapshot()
                .iter()
                .all(|s| s.active_generations == 0 && s.assigned_instances == 0),
        "pool not empty"
    );
    let state = pool.state();
    ensure!(
        state.next_placements.is_empty() && state.pending_rollovers.is_empty(),
        "pending placement remains"
    );
    Ok(())
}

async fn hp_probes(
    daemon: &DaemonInstanceHandle,
) -> Result<(CancellationToken, Vec<tokio::task::JoinHandle<()>>)> {
    let inner = daemon.inner.lock().await;
    let runtime = inner
        .control_runtime
        .as_ref()
        .context("missing control runtime")?;
    let token = CancellationToken::new();
    let tasks = inner
        .high_priority_runtime_pool
        .shard_handles()
        .into_iter()
        .map(|shard| {
            runtime.spawn(run_high_priority_shard_runtime_probe(
                inner.diagnostics.clone(),
                shard.shard_id,
                shard.handle,
                token.clone(),
            ))
        })
        .collect();
    Ok((token, tasks))
}

async fn stop_hp_probes(
    probes: (CancellationToken, Vec<tokio::task::JoinHandle<()>>),
) -> Result<()> {
    probes.0.cancel();
    for task in probes.1 {
        tokio::time::timeout(Duration::from_secs(5), task).await??;
    }
    Ok(())
}

async fn run_idle_case(output: &Path, seconds: u64, repetition: usize) -> Result<()> {
    let mut windows = Vec::new();
    windows.push(measure("before_daemon", 0, false, seconds).await?);
    let daemon = ServiceDaemon::builder()
        .with_registry(Registry::builder().with_tag("__idle_cost__").build())
        .build();
    daemon.run().await;
    // Replace only test probe ownership, using the same production probe functions.
    // Control/Standard probes and policy remain enabled in both HP-probe conditions.
    {
        let mut inner = daemon.inner.lock().await;
        for task in inner.runtime_probe_tasks.drain(..) {
            task.abort();
            match task.await {
                Ok(()) => {}
                Err(e) if e.is_cancelled() => {}
                Err(e) => return Err(e.into()),
            }
        }
        let control = inner
            .control_runtime
            .as_ref()
            .context("missing control runtime")?
            .handle()
            .clone();
        inner.spawn_runtime_probe(&control, RuntimeLane::Control);
        inner.spawn_runtime_probe(&tokio::runtime::Handle::current(), RuntimeLane::Standard);
    }
    let cap = daemon
        .inner
        .lock()
        .await
        .high_priority_runtime_pool
        .max_worker_threads()
        .min(12);
    let policy = format!(
        "{:?}",
        daemon
            .inner
            .lock()
            .await
            .high_priority_runtime_pool
            .policy()
    );
    ensure!(cap >= 2, "requires at least two available workers");
    for count in [1, cap] {
        {
            let mut inner = daemon.inner.lock().await;
            while inner.high_priority_runtime_pool.total_worker_threads() < count {
                // Materialize resource topology, not a test of scale-up decision quality.
                inner.high_priority_runtime_pool.create_shard(1)?;
            }
            inner
                .resources
                .runtime_facts
                .record_high_priority_shards(inner.high_priority_runtime_pool.snapshot());
        }
        warm_and_remove(&daemon, count).await?;
        let order = if repetition.is_multiple_of(2) {
            [false, true]
        } else {
            [true, false]
        };
        for enabled in order {
            let probes = if enabled {
                Some(hp_probes(&daemon).await?)
            } else {
                None
            };
            assert_empty(&daemon, count).await?;
            let pool_before = pool_evidence(&daemon).await;
            let mut window = measure(
                if count == 1 {
                    "initial_empty"
                } else {
                    "expanded_empty"
                },
                count,
                enabled,
                seconds,
            )
            .await?;
            assert_empty(&daemon, count).await?;
            let pool_after = pool_evidence(&daemon).await;
            let probes_completed = pool_after
                .completed_probes
                .checked_sub(pool_before.completed_probes)
                .context("probe counter reset")?;
            ensure!(
                if enabled {
                    probes_completed > 0
                } else {
                    probes_completed == 0
                },
                "probe state differs from experiment condition"
            );
            window.pool_before = Some(pool_before);
            window.pool_after = Some(pool_after);
            eprintln!(
                "measured {} workers={count} probes={enabled}: process={:.4}ms/s HP={:.4}ms/s RSS={}KiB",
                window.phase,
                window.process.cpu_ms_per_second,
                window.high_priority.cpu_ms_per_second,
                window.after.rss_kib
            );
            // Keep each completed window even if a later stage fails.
            write_json(
                &output.with_file_name(format!("window-{repetition}-{count}-{enabled}.json")),
                &window,
            )?;
            windows.push(window);
            if let Some(probes) = probes {
                stop_hp_probes(probes).await?;
            }
        }
    }
    daemon.shutdown();
    tokio::time::timeout(Duration::from_secs(10), daemon.wait()).await??;
    windows.push(measure("after_daemon_shutdown", 0, false, seconds).await?);
    let result = serde_json::json!({"schema":1, "repetition":repetition, "window_seconds":seconds,
        "expanded_workers":cap, "policy":policy, "profile":if cfg!(debug_assertions) { "debug" } else { "release" },
        "windows":windows, "cleanup":true});
    serde_json::to_writer_pretty(
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)?,
        &result,
    )?;
    Ok(())
}

#[test]
#[ignore = "Linux idle cost case; invoked in a fresh process by idle_cost_experiment"]
fn idle_cost_case() -> Result<()> {
    let seconds: u64 = std::env::var("SD_IDLE_SECONDS")?.parse()?;
    ensure!((1..=60).contains(&seconds), "invalid idle window");
    let repetition = std::env::var("SD_IDLE_REPETITION")?.parse()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("idle-standard")
        .enable_all()
        .build()?;
    runtime.block_on(run_idle_case(
        Path::new(&std::env::var("SD_IDLE_OUTPUT")?),
        seconds,
        repetition,
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ThreadSample {
    name: String,
    start_ticks: u64,
    cpu_ns: u64,
    voluntary: u64,
    involuntary: u64,
    state: String,
    wait_channel: String,
}

fn status_value<'a>(text: &'a str, key: &str) -> Result<&'a str> {
    text.lines()
        .find_map(|line| line.strip_prefix(key))
        .map(str::trim)
        .context(format!("missing proc field {key}"))
}

fn proc_number(text: &str, key: &str) -> Result<u64> {
    status_value(text, key)?
        .split_whitespace()
        .next()
        .context("empty proc value")?
        .parse()
        .map_err(Into::into)
}

fn thread_sample(
    status: &str,
    stat: &str,
    schedstat: &str,
    wait_channel: &str,
) -> Result<ThreadSample> {
    // /proc stat's parenthesized comm can itself contain spaces and ')'.
    let fields = stat.rsplit_once(") ").context("invalid task stat")?.1;
    Ok(ThreadSample {
        name: status_value(status, "Name:")?.into(),
        start_ticks: fields
            .split_whitespace()
            .nth(19)
            .context("missing starttime")?
            .parse()?,
        cpu_ns: schedstat
            .split_whitespace()
            .next()
            .context("missing schedstat CPU")?
            .parse()?,
        voluntary: proc_number(status, "voluntary_ctxt_switches:")?,
        involuntary: proc_number(status, "nonvoluntary_ctxt_switches:")?,
        state: status_value(status, "State:")?.into(),
        wait_channel: wait_channel.trim().into(),
    })
}

fn counter_delta(
    before: &BTreeMap<u32, ThreadSample>,
    after: &BTreeMap<u32, ThreadSample>,
) -> Result<(u64, u64, u64)> {
    ensure!(
        before.keys().eq(after.keys()),
        "thread set changed during measurement"
    );
    let mut sum = (0, 0, 0);
    for (tid, old) in before {
        let new = &after[tid];
        ensure!(
            old.start_ticks == new.start_ticks && old.name == new.name,
            "thread identity changed"
        );
        sum.0 += new
            .cpu_ns
            .checked_sub(old.cpu_ns)
            .context("CPU counter reset")?;
        sum.1 += new
            .voluntary
            .checked_sub(old.voluntary)
            .context("context switch counter reset")?;
        sum.2 += new
            .involuntary
            .checked_sub(old.involuntary)
            .context("context switch counter reset")?;
    }
    Ok(sum)
}

#[test]
fn idle_proc_parser_handles_spaces_parentheses_and_exact_fields() -> Result<()> {
    let status = "Name:\tsvc-high-priori\nState:\tS (sleeping)\nnonvoluntary_ctxt_switches:\t7\nvoluntary_ctxt_switches:\t11\n";
    let stat = format!("12 (worker ) name) S {} 12345", vec!["0"; 18].join(" "));
    let sample = thread_sample(status, &stat, "123456789 10 2", "futex_wait_queue")?;
    assert_eq!(sample.start_ticks, 12345);
    assert_eq!(sample.cpu_ns, 123456789);
    assert_eq!((sample.voluntary, sample.involuntary), (11, 7));
    assert_eq!(proc_number("Rss: 512 kB\nPss: 256 kB", "Pss:")?, 256);
    assert!(thread_sample(status, "bad", "", "").is_err());
    Ok(())
}

#[test]
fn idle_counter_delta_rejects_thread_churn_and_counter_reset() -> Result<()> {
    let sample = ThreadSample {
        name: "worker".into(),
        start_ticks: 5,
        cpu_ns: 10,
        voluntary: 20,
        involuntary: 2,
        state: "S".into(),
        wait_channel: "futex".into(),
    };
    let before = BTreeMap::from([(1, sample.clone())]);
    let mut after = before.clone();
    after.get_mut(&1).unwrap().cpu_ns += 30;
    after.get_mut(&1).unwrap().voluntary += 7;
    assert_eq!(counter_delta(&before, &after)?, (30, 7, 0));
    let measured = rates(&before, &after, 2.0)?;
    assert!((measured.cpu_ms_per_second - 0.000015).abs() < 1e-12);
    assert_eq!(measured.voluntary_per_second, 3.5);
    assert!(rates(&before, &after, 0.0).is_err());
    after.get_mut(&1).unwrap().start_ticks += 1;
    assert!(counter_delta(&before, &after).is_err());
    assert!(counter_delta(&before, &BTreeMap::new()).is_err());
    let mut reset = before.clone();
    reset.get_mut(&1).unwrap().cpu_ns = 1;
    assert!(counter_delta(&before, &reset).is_err());
    Ok(())
}
