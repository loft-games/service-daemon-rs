//! Typed acceptance evidence for production-default experiments.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Expected {
    pub mode: String,
    pub scenario: String,
    pub seconds: u64,
    pub blocking_work_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Verdict {
    Pass,
    SmokeOnly,
    Inconclusive,
    Fail,
}

#[derive(Debug, Serialize)]
pub(super) struct Summary {
    pub verdict: Verdict,
    pub reason: String,
    pub metrics: Metrics,
    pub generations: BTreeMap<String, Metrics>,
    pub max_workers: u64,
    pub requests: usize,
    pub linked_interventions: usize,
}

#[derive(Debug, Serialize)]
pub(super) struct Metrics {
    samples: usize,
    mean_ns: f64,
    p50_ns: u64,
    p95_ns: u64,
    p99_ns: u64,
    p999_ns: u64,
    max_ns: u64,
    low_sample_p99: bool,
    low_sample_p999: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
struct Instance(String);

#[derive(Debug, Deserialize)]
struct Record {
    at_ns: u64,
    #[serde(flatten)]
    data: Data,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
enum Data {
    Case {
        schema: u64,
        build: String,
        #[serde(flatten)]
        expected: Expected,
    },
    Policy(Policy),
    Generation(Generation),
    Sample(Sample),
    Resources {
        workers: u64,
    },
    Event(Box<Event>),
    Probes {},
    MeasurementEnd {},
    Cleanup {
        ok: bool,
    },
}

#[derive(Debug, Deserialize)]
struct Policy {
    max_workers: u64,
    high_avg_drift_ms: u64,
    minimum_completed_samples: usize,
    settle_ms: u64,
}

#[derive(Debug, Deserialize)]
struct Generation {
    instance: Instance,
    generation: u64,
    shard: Option<u64>,
    competitor: bool,
}

#[derive(Debug, Deserialize)]
struct Sample {
    instance: Instance,
    generation: u64,
    shard: Option<u64>,
    completed: bool,
    requested_ns: u64,
    drift_ns: u64,
    round_ns: u64,
}

#[derive(Debug, Deserialize)]
struct Event {
    message: String,
    service_instance_id: Option<Instance>,
    generation: Option<u64>,
    before_generation: Option<u64>,
    after_generation: Option<String>,
    source_shard: Option<String>,
    target_shard: Option<String>,
    actual_shard: Option<String>,
    metric: Option<String>,
    baseline: Option<u64>,
    before: Option<u64>,
    after: Option<String>,
    samples: Option<usize>,
    workers: Option<u64>,
    worker_threads: Option<u64>,
    reason: Option<String>,
}

const REQUEST: &str = "HighPriority resource intervention requested";
const EVALUATED: &str = "HighPriority intervention evaluated";
const PAUSED: &str = "HighPriority expansion paused; new comparable evidence is required";

fn number(text: Option<&str>, prefix: &str, suffix: &str) -> Result<u64> {
    text.context("missing numeric event field")?
        .strip_prefix(prefix)
        .and_then(|s| s.strip_suffix(suffix))
        .context("invalid numeric event field")?
        .parse()
        .context("invalid number")
}

fn metrics(samples: &[&Sample]) -> Metrics {
    let mut values: Vec<_> = samples.iter().map(|s| s.drift_ns).collect();
    values.sort_unstable();
    let percentile = |n: usize| {
        values
            .get((values.len() * n).div_ceil(1000).saturating_sub(1))
            .copied()
            .unwrap_or(0)
    };
    Metrics {
        samples: values.len(),
        mean_ns: mean(samples, |s| s.drift_ns),
        p50_ns: percentile(500),
        p95_ns: percentile(950),
        p99_ns: percentile(990),
        p999_ns: percentile(999),
        max_ns: values.last().copied().unwrap_or(0),
        low_sample_p99: values.len() < 100,
        low_sample_p999: values.len() < 1000,
    }
}

fn mean(samples: &[&Sample], value: impl Fn(&Sample) -> u64) -> f64 {
    samples.iter().map(|s| value(s) as f64).sum::<f64>() / samples.len().max(1) as f64
}

struct Intervention<'a> {
    instance: &'a Instance,
    before: u64,
    after: u64,
    shard: u64,
    at: u64,
    reason: &'a str,
}

pub(super) fn validate_json(bytes: &[u8], expected: &Expected, quick: bool) -> Result<Summary> {
    let mut records: Vec<Record> =
        serde_json::from_slice(bytes).context("invalid calibration records")?;
    ensure!(
        matches!(records.first().map(|r| &r.data), Some(Data::Case { .. })),
        "missing first case header"
    );
    ensure!(
        records
            .iter()
            .filter(|r| matches!(r.data, Data::Case { .. }))
            .count()
            == 1,
        "duplicate case header"
    );
    records.sort_by_key(|r| r.at_ns);
    let Some(Record {
        data:
            Data::Case {
                schema: 1,
                build,
                expected: header,
            },
        ..
    }) = records.first()
    else {
        bail!("missing or unsupported case header");
    };
    ensure!(
        header == expected && build == "release",
        "case header mismatch or non-release build"
    );
    ensure!(
        ["standard", "fixed", "adaptive"].contains(&header.mode.as_str()),
        "unknown mode"
    );
    ensure!(
        ["healthy", "contention", "low_benefit", "external_wait"]
            .contains(&header.scenario.as_str()),
        "unknown scenario"
    );
    ensure!(quick || header.seconds >= 90, "measurement too short");
    let ends: Vec<_> = records
        .iter()
        .filter(|r| matches!(r.data, Data::MeasurementEnd {}))
        .collect();
    let cleanup: Vec<_> = records
        .iter()
        .filter(|r| matches!(r.data, Data::Cleanup { .. }))
        .collect();
    ensure!(
        ends.len() == 1 && cleanup.len() == 1,
        "missing or duplicate measurement end/cleanup"
    );
    let end = ends[0].at_ns;
    ensure!(
        end >= header.seconds.saturating_mul(1_000_000_000),
        "truncated measurement"
    );
    ensure!(
        matches!(cleanup[0].data, Data::Cleanup { ok: true }) && cleanup[0].at_ns >= end,
        "failed or premature cleanup"
    );
    let measured = |r: &&Record| r.at_ns >= 5_000_000_000 && r.at_ns <= end;
    let samples: Vec<_> = records
        .iter()
        .filter(measured)
        .filter_map(|r| match &r.data {
            Data::Sample(s) if s.completed => Some(s),
            _ => None,
        })
        .collect();
    let mut placements = BTreeMap::new();
    for record in &records {
        if let Data::Generation(g) = &record.data {
            ensure!(
                placements
                    .insert((&g.instance, g.generation), (record.at_ns, g.shard))
                    .is_none(),
                "duplicate or contradictory generation"
            );
        }
    }
    for record in records.iter().filter(measured) {
        if let Data::Sample(s) = &record.data {
            let (started, shard) = placements
                .get(&(&s.instance, s.generation))
                .context("sample without actual generation")?;
            ensure!(
                *started <= record.at_ns && *shard == s.shard,
                "sample contradicts actual placement"
            );
        }
    }
    ensure!(!samples.is_empty(), "no completed samples");
    ensure!(
        samples
            .iter()
            .all(|s| s.round_ns >= s.requested_ns && s.requested_ns > 0),
        "invalid sample duration"
    );
    let resources: Vec<_> = records
        .iter()
        .filter_map(|r| match r.data {
            Data::Resources { workers } => Some(workers),
            _ => None,
        })
        .collect();
    let max_workers = resources
        .iter()
        .copied()
        .max()
        .context("missing resources")?;
    let requests: Vec<_> = records
        .iter()
        .filter(measured)
        .filter_map(|r| match &r.data {
            Data::Event(e) if e.message == REQUEST => Some((r.at_ns, e)),
            _ => None,
        })
        .collect();
    let effects: Vec<_> = records
        .iter()
        .filter(measured)
        .filter_map(|r| match &r.data {
            Data::Event(e) if e.message == EVALUATED || e.message == PAUSED => Some((r.at_ns, e)),
            _ => None,
        })
        .collect();
    let policies: Vec<_> = records
        .iter()
        .filter_map(|r| match &r.data {
            Data::Policy(p) => Some(p),
            _ => None,
        })
        .collect();
    let standard = Policy {
        max_workers: 0,
        high_avg_drift_ms: 0,
        minimum_completed_samples: 1,
        settle_ms: 0,
    };
    let policy = if header.mode == "standard" {
        ensure!(
            policies.is_empty() && requests.is_empty() && effects.is_empty(),
            "Standard has HighPriority control evidence"
        );
        &standard
    } else {
        ensure!(policies.len() == 1, "missing or duplicate policy");
        policies[0]
    };
    ensure!(max_workers <= policy.max_workers, "capacity exceeded");
    let threshold = policy.high_avg_drift_ms as f64 * 1_000_000.0;
    let mut used = BTreeSet::new();
    let mut linked = Vec::new();
    for (at, effect) in &effects {
        let instance = effect
            .service_instance_id
            .as_ref()
            .context("missing effect instance")?;
        let before = effect
            .before_generation
            .context("missing before generation")?;
        let after = number(effect.after_generation.as_deref(), "Some(", ")")?;
        let actual = number(
            effect.actual_shard.as_deref(),
            "Some(HighPriorityShardId(",
            "))",
        )?;
        let matching: Vec<_> = requests
            .iter()
            .enumerate()
            .filter(|(_, (_, req))| {
                req.service_instance_id.as_ref() == Some(instance) && req.generation == Some(before)
            })
            .collect();
        ensure!(matching.len() == 1, "missing or ambiguous request");
        let (index, (requested_at, request)) = matching[0];
        ensure!(used.insert(index), "duplicate evaluation");
        let source = number(request.source_shard.as_deref(), "hp#", "")?;
        let target = number(request.target_shard.as_deref(), "hp#", "")?;
        ensure!(
            request.workers.is_some_and(|n| n > 0 && n <= max_workers),
            "intervention capacity lacks observed resource evidence"
        );
        ensure!(
            actual == target && actual != source && after == before + 1,
            "incorrect placement/generation"
        );
        ensure!(
            effect.target_shard == request.target_shard
                && effect.before == request.baseline
                && request.baseline.is_some()
                && effect.worker_threads == request.workers
                && request.workers.is_some()
                && effect.metric == request.metric
                && request.metric.as_deref() == Some("service_sleep.mean_drift_ns"),
            "mismatched intervention identity"
        );
        let generation = |n, shard| -> Result<u64> {
            let rows: Vec<_> = records.iter().filter(|r| matches!(&r.data, Data::Generation(g) if &g.instance == instance && g.generation == n && g.shard == Some(shard) && !g.competitor)).collect();
            ensure!(rows.len() == 1, "missing or ambiguous actual generation");
            Ok(rows[0].at_ns)
        };
        let old_at = generation(before, source)?;
        let new_at = generation(after, actual)?;
        ensure!(
            old_at < *requested_at && *requested_at < new_at && new_at < *at,
            "invalid intervention chronology"
        );
        let evidence = |n, shard, start: u64, until: u64| -> Vec<&Sample> {
            records
                .iter()
                .filter_map(|r| match &r.data {
                    Data::Sample(s)
                        if s.completed
                            && &s.instance == instance
                            && s.generation == n
                            && s.shard == Some(shard)
                            && r.at_ns <= until
                            && r.at_ns >= until.saturating_sub(30_000_000_000)
                            && r.at_ns
                                .checked_sub(s.requested_ns)
                                .and_then(|n| n.checked_sub(s.drift_ns))
                                .is_some_and(|n| n >= start) =>
                    {
                        Some(s)
                    }
                    _ => None,
                })
                .collect()
        };
        let settle = policy.settle_ms * 1_000_000;
        let mut prior = evidence(before, source, old_at + settle, *requested_at);
        let count = request.samples.context("missing baseline count")?;
        ensure!(
            count >= policy.minimum_completed_samples && prior.len() >= count,
            "missing baseline samples"
        );
        prior.drain(..prior.len() - count);
        let post = evidence(after, actual, new_at + settle, *at);
        ensure!(
            post.len() >= policy.minimum_completed_samples,
            "missing post-intervention samples"
        );
        let before_mean = mean(&prior, |s| s.drift_ns);
        let after_mean = mean(&post, |s| s.drift_ns);
        let cadence = mean(&prior, |s| s.requested_ns);
        ensure!(
            (cadence - mean(&post, |s| s.requested_ns)).abs() <= cadence / 4.0,
            "incomparable cadence"
        );
        let logged_after = number(effect.after.as_deref(), "Some(", ")")? as f64;
        let logged_before = effect.before.context("missing before metric")? as f64;
        let reason = effect.reason.as_deref().context("missing outcome")?;
        let valid = match reason {
            "PressureCleared" => after_mean < threshold && logged_after < threshold,
            "Improved" => after_mean <= before_mean * 0.9 && logged_after <= logged_before * 0.9,
            "LowBenefit" | "PausedLowBenefit" => {
                after_mean >= threshold
                    && after_mean > before_mean * 0.9
                    && logged_after >= threshold
                    && logged_after > logged_before * 0.9
            }
            _ => false,
        };
        ensure!(
            before_mean >= threshold && valid,
            "samples contradict intervention outcome"
        );
        ensure!(
            (reason == "PausedLowBenefit") == (effect.message == PAUSED),
            "pause outcome mismatch"
        );
        linked.push(Intervention {
            instance,
            before,
            after,
            shard: actual,
            at: *at,
            reason,
        });
    }
    ensure!(
        used.len() == requests.len(),
        "request lacks complete evaluated evidence"
    );
    linked.sort_by_key(|i| i.at);
    let (verdict, reason) = if quick {
        (Verdict::SmokeOnly, "short smoke measurement")
    } else if header.mode != "adaptive"
        || ["healthy", "external_wait"].contains(&header.scenario.as_str())
    {
        if requests.is_empty() {
            (Verdict::Pass, "no unnecessary intervention")
        } else {
            (Verdict::Fail, "unexpected intervention")
        }
    } else if header.scenario == "contention" {
        if linked
            .iter()
            .any(|i| ["PressureCleared", "Improved"].contains(&i.reason))
        {
            (Verdict::Pass, "linked intervention has observed benefit")
        } else {
            (
                Verdict::Inconclusive,
                "no demonstrated beneficial intervention",
            )
        }
    } else {
        ensure!(linked.len() == 2, "low benefit requires two interventions");
        let first = &linked[0];
        let last = &linked[1];
        ensure!(
            first.instance == last.instance
                && first.after == last.before
                && first.reason == "LowBenefit"
                && last.reason == "PausedLowBenefit",
            "unlinked low-benefit sequence"
        );
        ensure!(
            max_workers < policy.max_workers,
            "resource cap masks low-benefit stop"
        );
        let post: Vec<_> = records
            .iter()
            .filter_map(|r| match &r.data {
                Data::Sample(s)
                    if s.completed
                        && &s.instance == last.instance
                        && s.generation == last.after
                        && s.shard == Some(last.shard)
                        && r.at_ns > last.at
                        && r.at_ns <= end =>
                {
                    Some((r.at_ns, s))
                }
                _ => None,
            })
            .collect();
        ensure!(
            post.len() >= 12
                && post
                    .iter()
                    .map(|(t, _)| *t)
                    .max()
                    .unwrap_or(0)
                    .saturating_sub(last.at)
                    >= 10_000_000_000,
            "missing sustained paused-instance observation"
        );
        ensure!(
            mean(&post.iter().map(|(_, s)| *s).collect::<Vec<_>>(), |s| s
                .drift_ns)
                >= threshold,
            "pressure cleared after pause"
        );
        (
            Verdict::Pass,
            "two low-benefit interventions stop below capacity under sustained pressure",
        )
    };
    let mut groups: BTreeMap<String, Vec<&Sample>> = BTreeMap::new();
    for s in &samples {
        groups
            .entry(format!("{}:{}", s.instance.0, s.generation))
            .or_default()
            .push(s);
    }
    Ok(Summary {
        verdict,
        reason: reason.into(),
        metrics: metrics(&samples),
        generations: groups
            .into_iter()
            .map(|(key, samples)| (key, metrics(&samples)))
            .collect(),
        max_workers,
        requests: requests.len(),
        linked_interventions: linked.len(),
    })
}

#[cfg(test)]
mod tests;
