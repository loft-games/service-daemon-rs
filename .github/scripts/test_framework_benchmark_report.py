"""Report adapter tests with Criterion-shaped files; no Cargo invocation."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("framework_benchmark_report.py")
CASES = [
    "observation/standard_steady",
    "diagnostics_snapshot/1", "diagnostics_snapshot/32", "diagnostics_snapshot/256",
    "provider_resolve/immutable_warm", "provider_resolve/managed_warm",
    "provider_resolve/arc_clone_drop_reference",
]


class ReportTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.summary = self.root / "summary.md"
        self.write(self.root / "metadata.json", {"commit": "fixture-commit"})
        for config in ("default", "high-priority"):
            cases = CASES + (["observation/high_priority_steady"] if config == "high-priority" else [])
            for case in cases:
                directory = self.root / config / case / "new"
                self.write(directory / "benchmark.json", {"full_id": case})
                self.write(directory / "sample.json", {"iters": [1, 2, 3], "times": [10, 20, 30]})
                estimate = {
                    "point_estimate": 10,
                    "confidence_interval": {
                        "confidence_level": 0.95, "lower_bound": 9, "upper_bound": 11,
                    },
                }
                self.write(directory / "estimates.json", {"mean": estimate, "median": estimate})

    def write(self, path, value):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value), encoding="utf-8")

    def run_report(self, default_status="success"):
        return subprocess.run(
            [sys.executable, str(SCRIPT), "report", str(self.root),
             "--default-status", default_status, "--high-priority-status", "success"],
            env={**os.environ, "GITHUB_STEP_SUMMARY": str(self.summary)},
            capture_output=True, text=True, check=False,
        )

    def test_complete_report_and_summary(self):
        result = self.run_report()
        self.assertEqual(result.returncode, 0, result.stderr)
        report = (self.root / "report.md").read_text()
        self.assertEqual(report, self.summary.read_text())
        self.assertIn("Overall result: **PASS**", report)
        self.assertIn("7/7", report)
        self.assertIn("8/8", report)
        self.assertIn("10.000", report)
        self.assertIn("95%", report)
        self.assertIn("fixture-commit", report)

    def test_missing_case_fails_but_preserves_other_configuration(self):
        (self.root / "default" / CASES[0] / "new/estimates.json").unlink()
        self.assertNotEqual(self.run_report().returncode, 0)
        report = self.summary.read_text()
        self.assertIn("6/7", report)
        self.assertIn("8/8", report)
        self.assertIn("FAILED", report)

    def test_command_failure_is_not_hidden_by_complete_files(self):
        self.assertNotEqual(self.run_report("failure").returncode, 0)
        self.assertIn("command: failure", self.summary.read_text())

    def test_invalid_statistics_fail(self):
        path = self.root / "default" / CASES[0] / "new/estimates.json"
        original = json.loads(path.read_text())
        for bad in (float("nan"), float("inf"), -1, True, "10"):
            with self.subTest(bad=bad):
                original["mean"]["point_estimate"] = bad
                self.write(path, original)
                self.assertNotEqual(self.run_report().returncode, 0)

    def test_malformed_json_and_sample_lengths_fail(self):
        path = self.root / "default" / CASES[0] / "new/sample.json"
        for value in ('{', '{"iters": [1,2], "times": [10]}', 'null'):
            with self.subTest(value=value):
                path.write_text(value)
                self.assertNotEqual(self.run_report().returncode, 0)
                self.assertTrue(self.summary.exists())

    def test_duplicate_or_unexpected_identity_fails(self):
        path = self.root / "default/extra/new/benchmark.json"
        for identity in (CASES[0], "unexpected/case"):
            with self.subTest(identity=identity):
                self.write(path, {"full_id": identity})
                self.assertNotEqual(self.run_report().returncode, 0)

    def test_old_baselines_are_not_current_results(self):
        for path in list((self.root / "default").glob("**/new")):
            path.rename(path.with_name("base"))
        self.assertNotEqual(self.run_report().returncode, 0)
        self.assertIn("0/7", self.summary.read_text())

    def test_missing_metadata_fails_with_report(self):
        (self.root / "metadata.json").unlink()
        self.assertNotEqual(self.run_report().returncode, 0)
        self.assertIn("metadata", self.summary.read_text())
        self.assertIn("Overall result: **FAILED**", self.summary.read_text())

    def test_invalid_confidence_interval_fails(self):
        path = self.root / "default" / CASES[0] / "new/estimates.json"
        original = json.loads(path.read_text())
        original["mean"]["confidence_interval"]["lower_bound"] = 12
        self.write(path, original)
        self.assertNotEqual(self.run_report().returncode, 0)
        self.assertIn("invalid confidence interval", self.summary.read_text())

    def test_summary_appends_and_escapes_metadata(self):
        self.summary.write_text("Existing summary\n")
        self.write(self.root / "metadata.json", {"commit": "<script>test</script>"})
        self.assertEqual(self.run_report().returncode, 0)
        summary = self.summary.read_text()
        self.assertTrue(summary.startswith("Existing summary\n"))
        self.assertNotIn("<script>", summary)

    def test_sample_count_is_not_iteration_count(self):
        path = self.root / "default" / CASES[0] / "new/sample.json"
        self.write(path, {"iters": [100, 200], "times": [1000, 2000]})
        self.assertEqual(self.run_report().returncode, 0)
        row = next(line for line in self.summary.read_text().splitlines()
                   if line.startswith("| observation/standard_steady |"))
        self.assertTrue(row.endswith("| 2 |"), row)


if __name__ == "__main__":
    unittest.main()
