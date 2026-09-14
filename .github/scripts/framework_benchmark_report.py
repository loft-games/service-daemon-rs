"""Present Criterion 0.8 JSON estimates in CI; statistical analysis stays in Criterion."""

import argparse
from datetime import datetime, timezone
import hashlib
import html
import json
import math
import os
from pathlib import Path
import platform
import subprocess


COMMON_CASES = (
    "observation/standard_steady",
    "diagnostics_snapshot/1", "diagnostics_snapshot/32", "diagnostics_snapshot/256",
    "provider_resolve/immutable_warm", "provider_resolve/managed_warm",
    "provider_resolve/arc_clone_drop_reference",
)
EXPECTED = {
    "default": COMMON_CASES,
    "high-priority": COMMON_CASES + ("observation/high_priority_steady",),
}


def read_object(path):
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"{path.name}: expected JSON object")
    return value


def positive(value):
    if type(value) not in (int, float) or not math.isfinite(value) or value <= 0:
        raise ValueError(f"expected finite positive number, got {value!r}")
    return value


def estimate(value):
    point = positive(value["point_estimate"])
    ci = value["confidence_interval"]
    low, high = positive(ci["lower_bound"]), positive(ci["upper_bound"])
    level = positive(ci["confidence_level"])
    if low > high or level >= 1:
        raise ValueError("invalid confidence interval")
    return point, low, high, level


def read_case(directory):
    sample = read_object(directory / "sample.json")
    iters, times = sample["iters"], sample["times"]
    if not isinstance(iters, list) or not isinstance(times, list):
        raise ValueError("sample iters and times must be arrays")
    if len(iters) < 2 or len(iters) != len(times):
        raise ValueError("sample lengths differ or contain fewer than two samples")
    for value in iters + times:
        positive(value)
    estimates = read_object(directory / "estimates.json")
    return estimate(estimates["mean"]), estimate(estimates["median"]), len(iters)


def escape(value):
    return html.escape(str(value)).replace("|", "&#124;").replace("\n", " ").replace("`", "&#96;")


def capture(root):
    def output(*command):
        return subprocess.check_output(command, text=True).strip()

    # Only record selected environment fields, never the complete CI environment.
    metadata = {
        "captured_at_utc": datetime.now(timezone.utc).isoformat(),
        "commit": output("git", "rev-parse", "HEAD"),
        "worktree": output("git", "status", "--porcelain"),
        "rustc": output("rustc", "-vV"),
        "cargo": output("cargo", "-V"),
        "os": platform.platform(),
        "cpu": output("lscpu"),
        "lockfile_sha256": hashlib.sha256(Path("Cargo.lock").read_bytes()).hexdigest(),
        "configuration": "bench profile; cfg(test)=true; Criterion default sampling parameters",
        "features": {"default": "default", "high-priority": "default + high-priority"},
        "ci": {key: os.environ.get(key, "") for key in (
            "GITHUB_SHA", "GITHUB_REF", "GITHUB_EVENT_NAME", "GITHUB_RUN_ID",
            "GITHUB_RUN_ATTEMPT", "RUNNER_OS", "RUNNER_ARCH", "ImageOS", "ImageVersion",
        )},
    }
    root.mkdir(parents=True, exist_ok=False)
    (root / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")


def report(root, statuses):
    errors = []
    lines = ["# Framework benchmarks", "",
             "Operation costs in ns/op (lower is faster). Mean and median are Criterion estimates;",
             "the interval is the confidence interval of the mean, not a latency percentile.", "",
             "These cfg(test) builds measure framework operations, not service latency or recovery.",
             "Default builds retain test-only probe counters. Hosted-runner noise limits comparisons;",
             "this report does not classify cross-run performance regressions.", ""]
    try:
        metadata = read_object(root / "metadata.json")
        if not metadata.get("commit"):
            raise ValueError("missing measured commit")
        lines += ["## Measurement environment", "", "<pre>",
                  html.escape(json.dumps(metadata, indent=2)), "</pre>", ""]
    except (OSError, ValueError) as error:
        errors.append(f"metadata: {error}")

    for config, expected in EXPECTED.items():
        rows = {}
        seen = set()
        config_errors = []
        if statuses[config] != "success":
            config_errors.append(f"{config} command: {statuses[config]}")
        # Never use Criterion's base/ or change/ directories as this run's evidence.
        for path in sorted((root / config).glob("**/new/benchmark.json")):
            try:
                identity = read_object(path)["full_id"]
                if not isinstance(identity, str) or identity not in expected:
                    raise ValueError(f"unexpected benchmark identity: {identity!r}")
                if identity in seen:
                    raise ValueError(f"duplicate benchmark identity: {identity}")
                seen.add(identity)
                rows[identity] = read_case(path.parent)
            except (OSError, ValueError, KeyError, TypeError) as error:
                config_errors.append(f"{path.relative_to(root)}: {error}")
        for identity in expected:
            if identity not in rows:
                config_errors.append(f"{config}: missing valid result for {identity}")
        state = "FAILED" if config_errors else "PASS"
        lines += [f"## {config}: {state} — {len(rows)}/{len(expected)} valid results", "",
                  f"Measurement command: {escape(statuses[config])}", "",
                  "| Benchmark | Mean (ns/op) | Mean CI (ns/op) | Median (ns/op) | Samples |",
                  "| :--- | ---: | :--- | ---: | ---: |"]
        for identity in expected:
            if identity in rows:
                mean, median, count = rows[identity]
                point, low, high, level = mean
                lines.append(f"| {identity} | {point:.3f} | {level:.0%}: [{low:.3f}, {high:.3f}] | {median[0]:.3f} | {count} |")
        lines.append("")
        errors.extend(config_errors)
    if errors:
        lines += ["## Report errors", ""] + [f"- {escape(error)}" for error in errors] + [""]
    lines[2:2] = [f"Overall result: **{'FAILED' if errors else 'PASS'}**", ""]
    lines += ["Download the framework-benchmarks artifact from this run for raw Criterion JSON,",
              "command logs, metadata.json and this report. Artifact retention: 30 days.", ""]
    text = "\n".join(lines)
    root.mkdir(parents=True, exist_ok=True)
    (root / "report.md").write_text(text, encoding="utf-8")
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(summary, "a", encoding="utf-8") as stream:
            stream.write(text)
    return 1 if errors else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("capture", "report"))
    parser.add_argument("root", type=Path)
    parser.add_argument("--default-status", default="unknown")
    parser.add_argument("--high-priority-status", default="unknown")
    args = parser.parse_args()
    if args.mode == "capture":
        capture(args.root)
        return 0
    return report(args.root, {"default": args.default_status, "high-priority": args.high_priority_status})


if __name__ == "__main__":
    raise SystemExit(main())
