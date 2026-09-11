//! Repeated natural pressure, removal and quiet valleys under a fixed budget.
use super::*;

const ROUNDS: usize = 6;
const CAPACITY: usize = 3;
const MIN_SECONDS: u64 = 390;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Shard {
    id: u64,
    active: usize,
    assigned: usize,
    pressure: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Snapshot {
    workers: usize,
    shards: Vec<Shard>,
    next_placements: usize,
    pending_rollovers: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Round {
    round: usize,
    instance: String,
    started_ns: u64,
    before: Snapshot,
    after: Snapshot,
    removed: Snapshot,
    quiet: Snapshot,
}

fn validate_counts(snapshot: &Snapshot, source_count: usize, target: Option<u64>) -> Result<()> {
    use std::collections::BTreeSet;
    ensure!(
        (1..=CAPACITY).contains(&snapshot.workers),
        "worker budget violated"
    );
    let ids: BTreeSet<_> = snapshot.shards.iter().map(|s| s.id).collect();
    ensure!(
        ids.len() == snapshot.workers
            && snapshot.shards.len() == snapshot.workers
            && ids.contains(&0),
        "shard topology/count mismatch"
    );
    ensure!(
        target.is_none_or(|id| id != 0 && ids.contains(&id)),
        "missing target"
    );
    ensure!(
        snapshot.next_placements == 0 && snapshot.pending_rollovers == 0,
        "pending placement/rollover survived round boundary"
    );
    for shard in &snapshot.shards {
        let expected = if shard.id == 0 {
            source_count
        } else {
            usize::from(target == Some(shard.id))
        };
        ensure!(
            shard.active == expected && shard.assigned == expected,
            "shard {} expected {expected} instances, got {}/{}",
            shard.id,
            shard.active,
            shard.assigned
        );
    }
    Ok(())
}

async fn snapshot(daemon: &DaemonInstanceHandle) -> Snapshot {
    let inner = daemon.inner.lock().await;
    let pool = &inner.high_priority_runtime_pool;
    Snapshot {
        workers: pool.total_worker_threads(),
        shards: pool
            .snapshot()
            .into_iter()
            .map(|s| Shard {
                id: s.shard_id.0,
                active: s.active_generations,
                assigned: s.assigned_instances,
                pressure: format!("{:?}", s.pressure_state),
            })
            .collect(),
        next_placements: pool.state().next_placements.len(),
        pending_rollovers: pool.state().pending_rollovers.len(),
    }
}

fn target_and_effect(records: &[Value], instance: &str) -> Result<(u64, Value, Value)> {
    let event = |message: &str| -> Result<Value> {
        let found: Vec<_> = records
            .iter()
            .filter(|r| {
                r["kind"] == "event"
                    && r["data"]["message"] == message
                    && r["data"]["service_instance_id"] == instance
            })
            .collect();
        ensure!(
            found.len() == 1,
            "missing or duplicate intervention for {instance}"
        );
        Ok(found[0].clone())
    };
    let request = event("HighPriority resource intervention requested")?;
    let effect = event("HighPriority intervention evaluated")?;
    let target = request["data"]["target_shard"]
        .as_str()
        .context("missing target")?
        .strip_prefix("hp#")
        .context("bad target")?
        .parse()?;
    Ok((target, request, effect))
}

fn validate_rounds(records: &[Value], rounds: &[Round], competitor: &str) -> Result<Value> {
    use std::collections::BTreeSet;
    ensure!(rounds.len() == ROUNDS, "incomplete cycle experiment");
    ensure!(
        rounds
            .iter()
            .map(|r| &r.instance)
            .collect::<BTreeSet<_>>()
            .len()
            == ROUNDS,
        "repeated subject identity"
    );
    let competitors: Vec<_> = records
        .iter()
        .filter(|r| r["kind"] == "generation" && r["data"]["instance"] == competitor)
        .collect();
    ensure!(
        competitors.len() == 1 && competitors[0]["data"]["shard"] == 0,
        "competitor did not stay on source"
    );
    let mut summaries = Vec::new();
    let mut reused = 0;
    let mut previous_workers = 1;
    for (index, round) in rounds.iter().enumerate() {
        ensure!(round.round == index + 1, "missing/reordered round");
        validate_counts(&round.before, ROUNDS - index + 1, None)?;
        let selected = select_instance(records, &round.instance);
        let evidence = evidence::validate_json(
            &serde_json::to_vec(&selected)?,
            &evidence::Expected {
                mode: "adaptive".into(),
                scenario: "contention".into(),
                seconds: MIN_SECONDS,
                blocking_work_ms: 400,
            },
            false,
        )?;
        ensure!(
            evidence.verdict == evidence::Verdict::Pass
                && evidence.requests == 1
                && evidence.linked_interventions == 1,
            "round lacks independent benefit"
        );
        let (target, request, effect) = target_and_effect(records, &round.instance)?;
        let requested_at = request["at_ns"].as_u64().context("missing request time")?;
        let effect_at = effect["at_ns"].as_u64().context("missing effect time")?;
        ensure!(
            requested_at > round.started_ns,
            "intervention preceded pressure"
        );
        ensure!(
            request["data"]["generation"] == 1 && request["data"]["source_shard"] == "hp#0",
            "unexpected source generation"
        );
        if index > 0 {
            let (_, _, prior) = target_and_effect(records, &rounds[index - 1].instance)?;
            ensure!(
                round
                    .started_ns
                    .saturating_sub(prior["at_ns"].as_u64().context("missing prior time")?)
                    >= 65_000_000_000,
                "quiet valley too short"
            );
        }
        validate_counts(&round.after, ROUNDS - index, Some(target))?;
        validate_counts(&round.removed, ROUNDS - index, None)?;
        validate_counts(&round.quiet, ROUNDS - index, None)?;
        ensure!(
            round.before.workers == previous_workers
                && round.after.workers == round.removed.workers
                && round.after.workers == round.quiet.workers,
            "unexplained resource growth/reclamation"
        );
        let nearest = records
            .iter()
            .rev()
            .find(|r| {
                r["kind"] == "resources" && r["at_ns"].as_u64().is_some_and(|t| t < requested_at)
            })
            .context("no pre-request resources")?;
        ensure!(
            requested_at - nearest["at_ns"].as_u64().context("bad time")? < 1_000_000_000,
            "stale resource observation"
        );
        let existing: Vec<_> = round.before.shards.iter().map(|s| s.id).collect();
        let empty: Vec<_> = round
            .before
            .shards
            .iter()
            .filter(|s| s.id != 0 && s.active == 0 && s.assigned == 0 && s.pressure == "Nominal")
            .map(|s| s.id)
            .collect();
        let outcome = if round.before.workers == CAPACITY {
            let valley = Valley {
                workers: CAPACITY,
                capacity: CAPACITY,
                existing,
                empty_nominal: empty,
            };
            ensure!(
                classify(&valley, target, round.after.workers)? == PlacementOutcome::ReusedEmpty,
                "full-budget round did not reuse"
            );
            ensure!(
                nearest["data"]["shards"]
                    .as_array()
                    .context("missing shards")?
                    .iter()
                    .any(|s| s["id"] == target
                        && s["active"] == 0
                        && s["assigned"] == 0
                        && s["pressure"] == "Nominal"),
                "target not empty/Nominal immediately before request"
            );
            reused += 1;
            "reused_empty"
        } else {
            ensure!(
                !existing.contains(&target) && round.after.workers == round.before.workers + 1,
                "growth phase did not add exactly one shard"
            );
            "allocated"
        };
        let source_pressure = records
            .iter()
            .find(|r| {
                r["kind"] == "probes"
                    && r["at_ns"]
                        .as_u64()
                        .is_some_and(|t| t > effect_at && t < effect_at + 5_000_000_000)
                    && r["data"]["shards"].as_array().is_some_and(|s| {
                        s.iter().any(|s| {
                            s["id"] == 0
                                && s["pressure"] == "Pressured"
                                && s["avg_drift_ms"].as_u64().is_some_and(|n| n >= 100)
                        })
                    })
            })
            .context("source pressure missing after intervention")?;
        previous_workers = round.after.workers;
        summaries.push(json!({"round":round.round,"instance":round.instance,"outcome":outcome,
            "target":target,"workers_before":round.before.workers,"workers_after":round.after.workers,
            "evidence":evidence,"request":request,"effect":effect,
            "before_request":nearest,"source_pressure_after":source_pressure}));
    }
    ensure!(
        reused == ROUNDS - (CAPACITY - 1),
        "missing plateau reuse rounds"
    );
    ensure!(
        records
            .iter()
            .filter(|r| r["kind"] == "resources")
            .all(|r| r["data"]["workers"]
                .as_u64()
                .is_some_and(|w| w <= CAPACITY as u64)),
        "budget exceeded between rounds"
    );
    Ok(
        json!({"rounds":summaries,"reused_rounds":reused,"workers_final":previous_workers,
        "outcome":"bounded_growth_then_reuse","cleanup":true}),
    )
}

async fn run_cycles(output: &Path) -> Result<()> {
    let daemon = ServiceDaemon::builder()
        .with_registry(Registry::builder().with_tag("__reuse_experiment__").build())
        .build();
    daemon.run().await;
    let mut rounds = Vec::new();
    let result = std::panic::AssertUnwindSafe(async {
        let resources = daemon.inner.lock().await.resources.clone();
        let handle = crate::core::context::__run_daemon_resources_sync_scope(resources, || {
            crate::service_handle!(reuse_worker)
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        {
            let inner = daemon.inner.lock().await;
            let pool = &inner.high_priority_runtime_pool;
            ensure!(
                pool.max_worker_threads() == CAPACITY && pool.total_worker_threads() == 1,
                "run with three CPU affinity; initial topology must have one shard"
            );
            record(
                "policy",
                json!({"max_workers":pool.max_worker_threads(),"settle_ms":2000,
                "high_avg_drift_ms":pool.policy().high_avg_drift_ms(),
                "minimum_completed_samples":pool.policy().minimum_completed_samples(),
                "control":format!("{:?}",pool.policy())}),
            );
        }
        let phase = Arc::new(AtomicU8::new(1));
        let mut subjects = Vec::new();
        for round in 0..ROUNDS {
            subjects.push(
                handle
                    .start(Input {
                        phase: phase.clone(),
                        role: Role::Round(round as u8),
                    })
                    .await?,
            );
        }
        let competitor = handle
            .start(Input {
                phase: phase.clone(),
                role: Role::Competitor,
            })
            .await?;
        let competitor_id = competitor.instance_id().to_string();
        save(
            &output.join("identities.json"),
            &json!({"competitor":competitor_id,
            "subjects":subjects.iter().map(|s|s.instance_id().to_string()).collect::<Vec<_>>()}),
        )?;
        observe_for(&daemon, 5).await;
        for (index, subject) in subjects.iter().enumerate() {
            let instance = subject.instance_id().to_string();
            let before = snapshot(&daemon).await;
            validate_counts(&before, ROUNDS - index + 1, None)?;
            let started_ns = START.get().unwrap().elapsed().as_nanos() as u64;
            eprintln!(
                "cycle {} pressure start, workers {}",
                index + 1,
                before.workers
            );
            phase.store((index * 2) as u8, Ordering::Release);
            wait_effect(&daemon, &instance).await?;
            observe_for(&daemon, 5).await;
            let after = snapshot(&daemon).await;
            phase.store(1, Ordering::Release);
            ensure!(subject.remove().await?, "subject not removed");
            let removed = snapshot(&daemon).await;
            validate_counts(&removed, ROUNDS - index, None)?;
            eprintln!(
                "cycle {} evaluated and removed, workers {}; quiet valley",
                index + 1,
                after.workers
            );
            observe_for(&daemon, if index + 1 < ROUNDS { 65 } else { 5 }).await;
            let quiet = snapshot(&daemon).await;
            rounds.push(Round {
                round: index + 1,
                instance,
                started_ns,
                before,
                after,
                removed,
                quiet,
            });
            save(
                &output.join(format!("round-{}.json", index + 1)),
                rounds.last().unwrap(),
            )?;
            save(
                &output.join(format!("through-round-{}.raw.json", index + 1)),
                &*RECORDS.lock().unwrap(),
            )?;
        }
        while START.get().unwrap().elapsed() < Duration::from_secs(MIN_SECONDS) {
            observe_for(&daemon, 1).await;
        }
        Ok::<_, anyhow::Error>(competitor_id)
    })
    .catch_unwind()
    .await;
    record("measurement_end", json!({}));
    daemon.shutdown();
    let cleanup = tokio::time::timeout(Duration::from_secs(10), daemon.wait()).await;
    record("cleanup", json!({"ok":matches!(cleanup,Ok(Ok(())))}));
    let records = RECORDS.lock().unwrap().clone();
    save(&output.join("raw.json"), &records)?;
    save(&output.join("rounds.json"), &rounds)?;
    ensure!(matches!(cleanup, Ok(Ok(()))), "cleanup failed");
    let competitor = match result {
        Ok(r) => r?,
        Err(p) => std::panic::resume_unwind(p),
    };
    save(
        &output.join("summary.json"),
        &validate_rounds(&records, &rounds, &competitor)?,
    )
}

#[test]
#[ignore = "six real pressure cycles, three-CPU taskset; fresh SD_REUSE_CASE"]
fn cycles_case() -> Result<()> {
    let output = std::path::PathBuf::from(std::env::var("SD_REUSE_CASE")?);
    fs::create_dir(&output)?;
    START.set(Instant::now()).unwrap();
    tracing::subscriber::set_global_default(TraceRegistry::default().with(Trace))?;
    ensure!(
        std::thread::available_parallelism()?.get() == CAPACITY,
        "three CPUs required"
    );
    record(
        "case",
        json!({"schema":1,"mode":"adaptive","scenario":"contention","seconds":MIN_SECONDS,
        "blocking_work_ms":400,"build":if cfg!(debug_assertions){"debug"}else{"release"},
        "available_parallelism":CAPACITY,"rounds":ROUNDS}),
    );
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?
        .block_on(run_cycles(&output))
}

#[test]
#[ignore = "read-only replay of an existing SD_REUSE_CASE; no runtime experiment"]
fn cycles_replay() -> Result<()> {
    let root = std::path::PathBuf::from(std::env::var("SD_REUSE_CASE")?);
    let records: Vec<Value> = serde_json::from_slice(&fs::read(root.join("raw.json"))?)?;
    let rounds: Vec<Round> = serde_json::from_slice(&fs::read(root.join("rounds.json"))?)?;
    let identities: Value = serde_json::from_slice(&fs::read(root.join("identities.json"))?)?;
    let summary = validate_rounds(
        &records,
        &rounds,
        identities["competitor"]
            .as_str()
            .context("missing competitor")?,
    )?;
    verify_summary_bytes(&summary, &fs::read(root.join("summary.json"))?)?;
    // Counterexamples must not turn a partial/misattributed run into success.
    ensure!(
        validate_rounds(
            &records,
            &rounds[..ROUNDS - 1],
            identities["competitor"].as_str().unwrap()
        )
        .is_err(),
        "missing round accepted"
    );
    let mut crossed = rounds.clone();
    crossed[ROUNDS - 1].instance = crossed[0].instance.clone();
    ensure!(
        validate_rounds(
            &records,
            &crossed,
            identities["competitor"].as_str().unwrap()
        )
        .is_err(),
        "crossed identity accepted"
    );
    Ok(())
}

fn verify_summary_bytes(summary: &Value, saved: &[u8]) -> Result<()> {
    // Compare the exact persisted representation. Parsing decimal means back
    // into f64 can change their last bit with the current JSON parser settings.
    // No numeric tolerance: any different regenerated report must fail.
    ensure!(
        serde_json::to_vec_pretty(summary)? == saved,
        "replay differs from stored result bytes"
    );
    Ok(())
}

#[test]
fn cycles_replay_preserves_serialized_means_without_float_reparse() -> Result<()> {
    for count in 1..=1200 {
        let summary = json!({"mean_ns":1_283_104_337.0 / f64::from(count),"verdict":"pass"});
        let bytes = serde_json::to_vec_pretty(&summary)?;
        verify_summary_bytes(&summary, &bytes)
            .with_context(|| format!("mean fixture count {count}: {summary}"))?;
        let changed = json!({"mean_ns":summary["mean_ns"],"verdict":"fail"});
        ensure!(
            verify_summary_bytes(&changed, &bytes).is_err(),
            "changed verdict accepted"
        );
    }
    Ok(())
}

#[test]
#[ignore = "archive corrected replay separately; SD_REUSE_DIR points to an existing experiment"]
fn cycles_archive_replay() -> Result<()> {
    use super::super::artifacts;
    use std::{collections::BTreeMap, process::Command};
    let original = std::path::PathBuf::from(std::env::var("SD_REUSE_DIR")?);
    ensure!(original.is_absolute(), "absolute experiment path required");
    let manifest_bytes = fs::read(original.join("manifest.json"))?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes)?;
    let original_sources: artifacts::Snapshot =
        serde_json::from_value(manifest["sources"].clone())?;
    artifacts::verify(&original.join("sources"), &original_sources)?;
    let verify_original = || -> Result<()> {
        ensure!(
            fs::read(original.join("manifest.json"))? == manifest_bytes,
            "original manifest changed"
        );
        for (path, hash) in manifest["artifacts"]
            .as_object()
            .context("missing artifacts")?
        {
            ensure!(
                artifacts::hash(&fs::read(original.join(path))?)
                    == hash.as_str().context("bad hash")?,
                "original artifact changed: {path}"
            );
        }
        Ok(())
    };
    verify_original()?;
    let output = original.join("replay-verifier");
    fs::create_dir(&output)?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("missing workspace")?;
    let sources = artifacts::snapshot(
        root,
        &output.join("sources"),
        &artifacts::source_paths(root)?,
    )?;
    save(
        &output.join("inputs.json"),
        &json!({"sources":sources,
        "original_manifest_sha256":artifacts::hash(&manifest_bytes),"original_source_id":original_sources.id}),
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
    ensure!(build.status.success(), "replay verifier build failed");
    let binary = String::from_utf8(build.stdout)?
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find_map(|v| {
            (v["reason"] == "compiler-artifact"
                && v["target"]["name"] == "service_daemon"
                && v["profile"]["test"] == true)
                .then(|| v["executable"].as_str().map(str::to_owned))
                .flatten()
        })
        .context("missing verifier executable")?;
    let executable = output.join("replay-test");
    fs::copy(binary, &executable)?;
    let mut results = Vec::new();
    for case in manifest["cases"].as_array().context("missing cases")? {
        let name = case["case"].as_str().context("missing case name")?;
        let result = Command::new(&executable)
            .args([
                "--exact",
                "core::service_daemon::high_priority::calibration::reuse::cycles::cycles_replay",
                "--ignored",
                "--nocapture",
            ])
            .env("SD_REUSE_CASE", original.join(name))
            .output()?;
        fs::write(output.join(format!("{name}.stdout")), &result.stdout)?;
        fs::write(output.join(format!("{name}.stderr")), &result.stderr)?;
        results.push(
            json!({"case":name,"success":result.status.success(),"exit_code":result.status.code()}),
        );
    }
    artifacts::verify(&output.join("sources"), &sources)?;
    verify_original()?;
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(&output)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            files.insert(
                entry
                    .file_name()
                    .to_str()
                    .context("bad filename")?
                    .to_owned(),
                artifacts::hash(&fs::read(entry.path())?),
            );
        }
    }
    let success = results.len() == 3 && results.iter().all(|r| r["success"] == true);
    save(
        &output.join("manifest.json"),
        &json!({"schema":1,"sources":sources,"artifacts":files,
        "original_manifest_sha256":artifacts::hash(&manifest_bytes),
        "binary_sha256":artifacts::hash(&fs::read(executable)?),"cases":results,"success":success}),
    )?;
    ensure!(
        success,
        "archived replay failed; original artifacts preserved"
    );
    Ok(())
}

#[test]
fn cycles_reject_stale_assignments_pending_placement_and_excess_budget() -> Result<()> {
    let good = Snapshot {
        workers: 3,
        shards: vec![
            Shard {
                id: 0,
                active: 4,
                assigned: 4,
                pressure: "Pressured".into(),
            },
            Shard {
                id: 1,
                active: 0,
                assigned: 0,
                pressure: "Nominal".into(),
            },
            Shard {
                id: 2,
                active: 0,
                assigned: 0,
                pressure: "Nominal".into(),
            },
        ],
        next_placements: 0,
        pending_rollovers: 0,
    };
    validate_counts(&good, 4, None)?;
    let mut bad = good.clone();
    bad.shards[1].assigned = 1;
    assert!(validate_counts(&bad, 4, None).is_err());
    bad = good.clone();
    bad.pending_rollovers = 1;
    assert!(validate_counts(&bad, 4, None).is_err());
    bad = good.clone();
    bad.next_placements = 1;
    assert!(validate_counts(&bad, 4, None).is_err());
    bad = good.clone();
    bad.workers = 4;
    assert!(validate_counts(&bad, 4, None).is_err());
    bad = good.clone();
    bad.shards[2].id = 1;
    assert!(validate_counts(&bad, 4, None).is_err());
    assert!(validate_counts(&good, 3, Some(1)).is_err());
    Ok(())
}
