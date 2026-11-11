//! Same-budget placement preference comparison; never a production policy knob.
use super::*;

pub(super) const PHASE_SECONDS: u64 = 50;
const POST_SECONDS: u64 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Arm {
    GrowthFirst,
    IdleFirst,
}

#[derive(Debug, Serialize)]
pub(super) struct Tails {
    samples: usize,
    p99_ns: u64,
    p999_ns: u64,
    max_ns: u64,
    low_sample_p999: bool,
}

fn tails(mut values: Vec<u64>) -> Result<Tails> {
    ensure!(
        values.len() >= 1000,
        "insufficient fixed-window tail samples"
    );
    values.sort_unstable();
    let n = values.len();
    Ok(Tails {
        samples: n,
        p99_ns: values[(n * 99).div_ceil(100) - 1],
        p999_ns: values[(n * 999).div_ceil(1000) - 1],
        max_ns: values[n - 1],
        low_sample_p999: n < 10_000,
    })
}

pub(super) fn window(
    records: &[Value],
    instance: &str,
    generation: Option<u64>,
    start: u64,
    end: u64,
) -> Result<Tails> {
    let mut values = Vec::new();
    for r in records.iter().filter(|r| {
        r["kind"] == "sample"
            && r["data"]["instance"] == instance
            && r["data"]["completed"] == true
            && generation.is_none_or(|g| r["data"]["generation"] == g)
    }) {
        let at = r["at_ns"].as_u64().context("missing sample time")?;
        let drift = r["data"]["drift_ns"].as_u64().context("missing drift")?;
        let requested = r["data"]["requested_ns"]
            .as_u64()
            .context("missing cadence")?;
        let began = at
            .checked_sub(drift)
            .and_then(|t| t.checked_sub(requested))
            .context("invalid sample duration")?;
        if began >= start && at <= end {
            values.push(drift);
        }
    }
    tails(values)
}

pub(super) fn metrics(records: &[Value], checkpoints: &[Value], arm: Arm) -> Result<Value> {
    ensure!(checkpoints.len() == 1, "ambiguous A/B checkpoint");
    let c = &checkpoints[0];
    ensure!(
        c["placement_arm"] == serde_json::to_value(arm)?,
        "checkpoint arm mismatch"
    );
    ensure!(
        records[0]["kind"] == "case"
            && records[0]["data"]["placement_arm"] == c["placement_arm"]
            && records[0]["data"]["available_parallelism"] == 3,
        "A/B parameters differ"
    );
    let second = c["second"].as_str().context("missing second subject")?;
    let onset = c["second_pressure_at_ns"]
        .as_u64()
        .context("missing onset")?;
    let generations: Vec<_> = records
        .iter()
        .filter(|r| {
            r["kind"] == "generation"
                && r["data"]["instance"] == second
                && r["data"]["generation"] == 2
        })
        .collect();
    ensure!(
        generations.len() == 1,
        "missing/duplicate second generation"
    );
    let generation = generations[0];
    let target = if arm == Arm::IdleFirst { 1 } else { 2 };
    ensure!(
        generation["data"]["shard"] == target,
        "unexpected A/B placement"
    );
    let gen_at = generation["at_ns"]
        .as_u64()
        .context("missing generation time")?;
    let events: Vec<_> = records
        .iter()
        .filter(|r| {
            r["kind"] == "event"
                && r["data"]["service_instance_id"] == second
                && r["data"]["message"] == "HighPriority intervention evaluated"
        })
        .collect();
    ensure!(events.len() == 1, "missing/duplicate effect");
    let effect_at = events[0]["at_ns"].as_u64().context("missing effect time")?;
    ensure!(
        onset < gen_at && gen_at < effect_at,
        "invalid recovery chronology"
    );
    let phase_end = onset + PHASE_SECONDS * 1_000_000_000;
    let post_start = gen_at + 2_000_000_000;
    let post_end = post_start + POST_SECONDS * 1_000_000_000;
    ensure!(
        post_end <= phase_end,
        "recovery too late for complete fixed post window"
    );
    ensure!(
        records.iter().any(|r| r["kind"] == "measurement_end"
            && r["at_ns"].as_u64().is_some_and(|t| t >= phase_end)),
        "incomplete fixed phase"
    );
    let peak = records
        .iter()
        .filter(|r| r["kind"] == "resources")
        .map(|r| r["data"]["workers"].as_u64().context("missing workers"))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .max()
        .context("missing resources")?;
    ensure!(
        peak == if arm == Arm::IdleFirst { 2 } else { 3 },
        "A/B resource behavior differs"
    );
    Ok(
        json!({"arm":arm,"peak_workers":peak,"generation_delay_ns":gen_at-onset,
        "evaluation_delay_ns":effect_at-onset,"phase_seconds":PHASE_SECONDS,"post_seconds":POST_SECONDS,
        "post_start_ns":post_start,"post_end_ns":post_end,
        "phase":window(records,second,None,onset,phase_end)?,
        "post_generation":window(records,second,Some(2),post_start,post_end)?}),
    )
}

fn rebuild(records: &[Value], checkpoints: &[Value]) -> Result<Value> {
    ensure!(checkpoints.len() == 1, "missing checkpoint");
    let c = &checkpoints[0];
    let arm: Arm = serde_json::from_value(c["placement_arm"].clone())?;
    let valley: Valley = serde_json::from_value(c["valley"].clone())?;
    ensure!(
        valley.capacity == 3
            && valley.workers == 2
            && valley.existing == [0, 1]
            && valley.empty_nominal == [1],
        "initial topology mismatch"
    );
    let mut summary = validate_case(
        records,
        c["first"].as_str().context("missing first")?,
        c["second"].as_str().context("missing second")?,
        c["competitor"].as_str().context("missing competitor")?,
        &valley,
        if arm == Arm::IdleFirst { 2 } else { 3 },
    )?;
    summary["comparison"] = metrics(records, checkpoints, arm)?;
    Ok(summary)
}

#[test]
#[ignore = "same-budget A/B case; taskset three CPUs; SD_REUSE_CASE and SD_PLACEMENT_ARM"]
fn ab_case() -> Result<()> {
    let arm = match std::env::var("SD_PLACEMENT_ARM")?.as_str() {
        "growth_first" => Arm::GrowthFirst,
        "idle_first" => Arm::IdleFirst,
        _ => anyhow::bail!("invalid arm"),
    };
    let output = std::path::PathBuf::from(std::env::var("SD_REUSE_CASE")?);
    fs::create_dir(&output)?;
    START.set(Instant::now()).unwrap();
    tracing::subscriber::set_global_default(TraceRegistry::default().with(Trace))?;
    record(
        "case",
        json!({"schema":1,"mode":"adaptive","scenario":"contention","seconds":90,
        "blocking_work_ms":400,"build":if cfg!(debug_assertions){"debug"}else{"release"},
        "available_parallelism":std::thread::available_parallelism()?.get(),"placement_arm":arm}),
    );
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?
        .block_on(run_case(&output, Some(arm)))
}

#[test]
#[ignore = "read-only complete A/B replay; SD_REUSE_CASE"]
fn ab_replay() -> Result<()> {
    let root = std::path::PathBuf::from(std::env::var("SD_REUSE_CASE")?);
    let records: Vec<Value> = serde_json::from_slice(&fs::read(root.join("raw.json"))?)?;
    let checkpoints: Vec<Value> =
        serde_json::from_slice(&fs::read(root.join("checkpoints.json"))?)?;
    ensure!(
        serde_json::to_vec_pretty(&rebuild(&records, &checkpoints)?)?
            == fs::read(root.join("summary.json"))?,
        "report byte mismatch"
    );
    let mut wrong = checkpoints.clone();
    wrong[0]["placement_arm"] = json!(if checkpoints[0]["placement_arm"] == "idle_first" {
        "growth_first"
    } else {
        "idle_first"
    });
    ensure!(
        rebuild(&records, &wrong).is_err(),
        "mismatched arm accepted"
    );
    let missing: Vec<_> = records
        .iter()
        .filter(|r| {
            !(r["kind"] == "generation"
                && r["data"]["instance"] == checkpoints[0]["second"]
                && r["data"]["generation"] == 2)
        })
        .cloned()
        .collect();
    ensure!(
        rebuild(&missing, &checkpoints).is_err(),
        "missing actual placement accepted"
    );
    Ok(())
}

#[test]
fn fixed_window_tails_keep_boundary_and_low_sample_semantics() -> Result<()> {
    let t = tails((1..=1000).collect())?;
    assert_eq!((t.p99_ns, t.p999_ns, t.max_ns), (990, 999, 1000));
    assert!(t.low_sample_p999);
    assert!(tails(vec![1; 999]).is_err());
    assert!(!tails(vec![1; 10_000])?.low_sample_p999);
    let samples: Vec<_> = (1..=1001)
        .map(|n| {
            json!({"kind":"sample","at_ns":n+10,
        "data":{"instance":"i","generation":2,"completed":true,"requested_ns":1,"drift_ns":1}})
        })
        .collect();
    assert_eq!(window(&samples, "i", Some(2), 10, 1011)?.samples, 1000);
    assert!(window(&samples, "other", Some(2), 10, 1011).is_err());
    Ok(())
}

pub(super) fn comparison_report(root: &Path, cases: &[Value]) -> Result<String> {
    use std::fmt::Write;
    let mut report = String::from(
        "\n## Same-budget A/B measurements\n\nBoth arms use the same three-CPU affinity (see exits), inferred capacity 3 and the same executable. The second phase is 50 seconds; post-generation statistics cover exactly 30 seconds after 2-second settling. P99.9 is exploratory when fewer than 10,000 samples. Four pairs cannot establish statistical non-inferiority.\n\n| Case | Peak workers | Generation delay (ms) | Evaluation delay (ms) | Post P99 (ms) | Post P99.9 (ms) | Phase P99.9 (ms) |\n|---|---:|---:|---:|---:|---:|---:|\n",
    );
    for case in cases {
        let name = case["case"].as_str().context("missing case")?;
        if case["success"] != true {
            writeln!(
                report,
                "| {name} | Insufficient evidence | - | - | - | - | - |"
            )?;
            continue;
        }
        let summary: Value =
            serde_json::from_slice(&fs::read(root.join(name).join("summary.json"))?)?;
        let m = &summary["comparison"];
        let ms =
            |v: &Value| -> Result<f64> { Ok(v.as_u64().context("missing metric")? as f64 / 1e6) };
        writeln!(
            report,
            "| {name} | {} | {:.3} | {:.3} | {:.6} | {:.6} | {:.6} |",
            m["peak_workers"],
            ms(&m["generation_delay_ns"])?,
            ms(&m["evaluation_delay_ns"])?,
            ms(&m["post_generation"]["p99_ns"])?,
            ms(&m["post_generation"]["p999_ns"])?,
            ms(&m["phase"]["p999_ns"])?
        )?;
    }
    Ok(report)
}
