use super::{Expected, Verdict, validate_json};
use serde_json::{Value, json};

#[test]
fn standard_has_no_high_priority_policy() {
    let mut rows = base("healthy");
    rows.retain(|r| r["kind"] != "policy");
    rows[0]["data"]["mode"] = json!("standard");
    for r in &mut rows {
        if r["kind"] == "resources" {
            r["data"]["workers"] = json!(0);
        }
    }
    rows.push(sample(1, 6, 1_000_000));
    let mut requested = expected("healthy");
    requested.mode = "standard".into();
    assert_eq!(
        validate_json(&serde_json::to_vec(&rows).unwrap(), &requested, false)
            .unwrap()
            .verdict,
        Verdict::Pass
    );
}

const REQUEST: &str = "HighPriority resource intervention requested";
const EVALUATED: &str = "HighPriority intervention evaluated";
const PAUSED: &str = "HighPriority expansion paused; new comparable evidence is required";

fn expected(scenario: &str) -> Expected {
    Expected {
        mode: "adaptive".into(),
        scenario: scenario.into(),
        seconds: 90,
        blocking_work_ms: 250,
    }
}

fn row(kind: &str, seconds: u64, data: Value) -> Value {
    json!({"kind": kind, "at_ns": seconds * 1_000_000_000, "data": data})
}

fn generation(number: u64, seconds: u64) -> Value {
    row(
        "generation",
        seconds,
        json!({
            "instance": "subject", "generation": number,
            "shard": number - 1, "competitor": false,
        }),
    )
}

fn sample(number: u64, seconds: u64, drift: u64) -> Value {
    row(
        "sample",
        seconds,
        json!({
            "instance": "subject", "generation": number, "shard": number - 1,
            "completed": true, "drift_ns": drift, "requested_ns": 5_000_000,
            "round_ns": drift + 5_000_000,
        }),
    )
}

fn base(scenario: &str) -> Vec<Value> {
    vec![
        row(
            "case",
            0,
            json!({
                "schema": 1, "mode": "adaptive", "scenario": scenario,
                "seconds": 90, "build": "release", "blocking_work_ms": 250,
                "warmup_seconds": 5, "body_workers": 1, "control_workers": 1,
            }),
        ),
        row(
            "policy",
            0,
            json!({
                "max_workers": 12, "initial_workers": 1, "high_avg_drift_ms": 100,
                "minimum_completed_samples": 3, "settle_ms": 2000,
            }),
        ),
        row("resources", 0, json!({"workers": 1})),
        generation(1, 1),
        row("measurement_end", 90, json!({})),
        row("cleanup", 91, json!({"ok": true})),
    ]
}

fn append_intervention(rows: &mut Vec<Value>, before: u64, at: u64, reason: &str) {
    let after_drift = if reason == "PressureCleared" {
        1_000_000
    } else {
        245_000_000
    };
    rows.push(row(
        "event",
        at,
        json!({
            "message": REQUEST, "service_instance_id": "subject", "generation": before,
            "source_shard": format!("hp#{}", before - 1),
            "target_shard": format!("hp#{before}"), "metric": "service_sleep.mean_drift_ns",
            "baseline": 245_000_000, "samples": 3, "workers": before + 1,
        }),
    ));
    rows.push(row("resources", at, json!({"workers": before + 1})));
    rows.push(generation(before + 1, at + 1));
    for seconds in at + 4..=at + 6 {
        rows.push(sample(before + 1, seconds, after_drift));
    }
    rows.push(row(
        "event",
        at + 10,
        json!({
            "message": if reason == "PausedLowBenefit" { PAUSED } else { EVALUATED },
            "service_instance_id": "subject", "before_generation": before,
            "after_generation": format!("Some({})", before + 1),
            "actual_shard": format!("Some(HighPriorityShardId({before}))"),
            "target_shard": format!("hp#{before}"), "worker_threads": before + 1,
            "metric": "service_sleep.mean_drift_ns", "before": 245_000_000,
            "after": format!("Some({after_drift})"), "reason": reason,
        }),
    ));
}

fn complete(scenario: &str) -> Vec<Value> {
    let mut rows = base(scenario);
    for seconds in 6..=8 {
        rows.push(sample(1, seconds, 245_000_000));
    }
    append_intervention(
        &mut rows,
        1,
        10,
        if scenario == "low_benefit" {
            "LowBenefit"
        } else {
            "PressureCleared"
        },
    );
    if scenario == "low_benefit" {
        for seconds in 55..=57 {
            rows.push(sample(2, seconds, 245_000_000));
        }
        append_intervention(&mut rows, 2, 60, "PausedLowBenefit");
        for seconds in 72..=89 {
            rows.push(sample(3, seconds, 245_000_000));
        }
    }
    rows.sort_by_key(|item| item["at_ns"].as_u64().unwrap());
    rows
}

fn assert_rejected(rows: &[Value], scenario: &str) {
    let result = validate_json(
        &serde_json::to_vec(rows).unwrap(),
        &expected(scenario),
        false,
    );
    if let Ok(summary) = result {
        assert_ne!(summary.verdict, Verdict::Pass, "{}", summary.reason);
    }
}

#[test]
fn complete_contention_chain_with_observed_improvement_passes() {
    let rows = complete("contention");
    let summary = validate_json(
        &serde_json::to_vec(&rows).unwrap(),
        &expected("contention"),
        false,
    )
    .unwrap();
    assert_eq!(summary.verdict, Verdict::Pass, "{}", summary.reason);
}

#[test]
fn two_linked_low_benefit_interventions_and_persistent_pressure_below_cap_pass() {
    let rows = complete("low_benefit");
    let summary = validate_json(
        &serde_json::to_vec(&rows).unwrap(),
        &expected("low_benefit"),
        false,
    )
    .unwrap();
    assert_eq!(summary.verdict, Verdict::Pass, "{}", summary.reason);
}

#[test]
fn quick_measurements_never_claim_full_calibration() {
    let rows = complete("contention");
    let summary = validate_json(
        &serde_json::to_vec(&rows).unwrap(),
        &expected("contention"),
        true,
    )
    .unwrap();
    assert_eq!(summary.verdict, Verdict::SmokeOnly);
}

#[test]
fn review_reproduction_low_benefit_logs_without_requests_or_generations_are_rejected() {
    let mut rows = base("low_benefit");
    rows.retain(|item| item["kind"] != "generation");
    rows.push(row(
        "event",
        20,
        json!({"message": EVALUATED, "reason": "LowBenefit"}),
    ));
    rows.push(row(
        "event",
        70,
        json!({"message": PAUSED, "reason": "PausedLowBenefit"}),
    ));
    for seconds in 72..=89 {
        rows.push(sample(3, seconds, 245_000_000));
    }
    assert_rejected(&rows, "low_benefit");
}

#[test]
fn review_reproduction_unidentified_contention_logs_without_placement_or_after_samples_are_rejected()
 {
    let mut rows = base("contention");
    rows.retain(|item| item["kind"] != "generation");
    rows.push(sample(1, 6, 245_000_000));
    rows.push(row("event", 10, json!({"message": REQUEST})));
    rows.push(row(
        "event",
        20,
        json!({"message": EVALUATED, "reason": "Improved"}),
    ));
    assert_rejected(&rows, "contention");
}

#[test]
fn evaluation_without_matching_request_is_rejected() {
    let mut rows = complete("contention");
    rows.retain(|item| item["data"]["message"] != REQUEST);
    assert_rejected(&rows, "contention");
}

#[test]
fn missing_actual_generation_start_is_rejected() {
    let mut rows = complete("contention");
    rows.retain(|item| !(item["kind"] == "generation" && item["data"]["generation"] == 2));
    assert_rejected(&rows, "contention");
}

#[test]
fn evaluation_without_observed_after_samples_is_rejected() {
    let mut rows = complete("contention");
    rows.retain(|item| !(item["kind"] == "sample" && item["data"]["generation"] == 2));
    assert_rejected(&rows, "contention");
}

#[test]
fn evaluation_without_observed_before_samples_is_rejected() {
    let mut rows = complete("contention");
    rows.retain(|item| !(item["kind"] == "sample" && item["data"]["generation"] == 1));
    assert_rejected(&rows, "contention");
}

#[test]
fn evaluation_from_another_instance_cannot_complete_request() {
    let mut rows = complete("contention");
    for item in &mut rows {
        if item["data"]["message"] == EVALUATED {
            item["data"]["service_instance_id"] = json!("other-instance");
        }
    }
    assert_rejected(&rows, "contention");
}

#[test]
fn samples_from_another_instance_cannot_supply_after_evidence() {
    let mut rows = complete("contention");
    for item in &mut rows {
        if item["kind"] == "sample" && item["data"]["generation"] == 2 {
            item["data"]["instance"] = json!("other-instance");
        }
    }
    assert_rejected(&rows, "contention");
}

#[test]
fn samples_from_another_generation_cannot_supply_after_evidence() {
    let mut rows = complete("contention");
    for item in &mut rows {
        if item["kind"] == "sample" && item["data"]["generation"] == 2 {
            item["data"]["generation"] = json!(3);
        }
    }
    assert_rejected(&rows, "contention");
}

#[test]
fn requested_shard_is_not_proof_of_actual_placement() {
    let mut rows = complete("contention");
    for item in &mut rows {
        if item["data"]["generation"] == 2 {
            item["data"]["shard"] = json!(0);
        }
    }
    assert_rejected(&rows, "contention");
}

#[test]
fn after_samples_must_follow_generation_settling() {
    let mut rows = complete("contention");
    for item in &mut rows {
        if item["kind"] == "sample" && item["data"]["generation"] == 2 {
            item["at_ns"] = json!(12_000_000_000u64);
        }
    }
    assert_rejected(&rows, "contention");
}

#[test]
fn improvement_log_cannot_override_observed_persistent_pressure() {
    let mut rows = complete("contention");
    for item in &mut rows {
        if item["kind"] == "sample" && item["data"]["generation"] == 2 {
            item["data"]["drift_ns"] = json!(245_000_000);
            item["data"]["round_ns"] = json!(250_000_000);
        }
    }
    assert_rejected(&rows, "contention");
}

#[test]
fn low_benefit_requires_two_actual_interventions() {
    let mut rows = complete("low_benefit");
    rows.retain(|item| !(item["data"]["message"] == REQUEST && item["data"]["generation"] == 1));
    assert_rejected(&rows, "low_benefit");
}

#[test]
fn pause_pressure_must_come_from_paused_instance() {
    let mut rows = complete("low_benefit");
    for item in &mut rows {
        if item["kind"] == "sample" && item["at_ns"].as_u64().unwrap() > 70_000_000_000 {
            item["data"]["instance"] = json!("unrelated-busy-instance");
        }
    }
    assert_rejected(&rows, "low_benefit");
}

#[test]
fn two_low_benefit_outcomes_from_different_instances_cannot_prove_convergence() {
    let mut rows = complete("low_benefit");
    // Keep each intervention locally complete; only the claimed two-attempt
    // history is invalid because the second attempt belongs to another service.
    let mut other_generation = generation(2, 51);
    other_generation["data"]["instance"] = json!("second-subject");
    rows.push(other_generation);
    for item in &mut rows {
        if item["at_ns"].as_u64().unwrap() >= 51_000_000_000 {
            if item["data"].get("instance").is_some() {
                item["data"]["instance"] = json!("second-subject");
            }
            if item["data"].get("service_instance_id").is_some() {
                item["data"]["service_instance_id"] = json!("second-subject");
            }
        }
    }
    rows.sort_by_key(|item| item["at_ns"].as_u64().unwrap());
    assert_rejected(&rows, "low_benefit");
}

#[test]
fn pressure_after_pause_must_use_the_recorded_policy_threshold() {
    let mut rows = complete("low_benefit");
    rows.iter_mut()
        .find(|item| item["kind"] == "policy")
        .unwrap()["data"]["high_avg_drift_ms"] = json!(300);
    assert_rejected(&rows, "low_benefit");
}

#[test]
fn resource_cap_cannot_masquerade_as_low_benefit_stop() {
    let mut rows = complete("low_benefit");
    rows.iter_mut()
        .find(|item| item["kind"] == "policy")
        .unwrap()["data"]["max_workers"] = json!(3);
    assert_rejected(&rows, "low_benefit");
}

#[test]
fn case_header_must_match_each_requested_parameter() {
    for (field, replacement) in [
        ("mode", json!("fixed")),
        ("scenario", json!("healthy")),
        ("seconds", json!(91)),
        ("blocking_work_ms", json!(400)),
    ] {
        let mut rows = complete("contention");
        rows.iter_mut().find(|item| item["kind"] == "case").unwrap()["data"][field] = replacement;
        assert_rejected(&rows, "contention");
    }
}

#[test]
fn missing_or_failed_cleanup_is_rejected() {
    let mut missing = complete("contention");
    missing.retain(|item| item["kind"] != "cleanup");
    assert_rejected(&missing, "contention");
    let mut failed = complete("contention");
    failed
        .iter_mut()
        .find(|item| item["kind"] == "cleanup")
        .unwrap()["data"]["ok"] = json!(false);
    assert_rejected(&failed, "contention");
}

#[test]
fn fewer_than_policy_minimum_after_samples_is_rejected() {
    let mut rows = complete("contention");
    rows.retain(|item| !(item["kind"] == "sample" && item["at_ns"] == 16_000_000_000u64));
    assert_rejected(&rows, "contention");
}
