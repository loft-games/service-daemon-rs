"""Regression coverage for Cargo artifact discovery, independent of its layout."""

import contextlib
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import linkme_smoke_binary as smoke


def artifact(path, **fields):
    return {
        "reason": "compiler-artifact",
        "target": {"name": "linkme_smoke", "kind": ["test"]},
        "profile": {"test": True},
        "executable": str(path),
        **fields,
    }


class BinaryTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def create_binary(self, relative):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.touch()
        return path

    def cargo_output(self, *messages):
        return patch.object(smoke.subprocess, "run", return_value=subprocess.CompletedProcess(
            [], 0, "\n".join(json.dumps(message) for message in messages),
        ))

    def test_old_new_and_custom_target_directories(self):
        for relative in (
            "target/release/deps/linkme_smoke-old",
            "target/x86_64-pc-windows-gnu/release/build/service-daemon/hash/out/linkme_smoke-hash.exe",
            "custom target directory/build/service-daemon/hash/out/linkme_smoke.exe",
        ):
            with self.subTest(relative=relative):
                binary = self.create_binary(relative)
                # Unrelated artifacts must not win even if they have executables.
                unrelated = artifact("wrong", target={"name": "other", "kind": ["test"]})
                with self.cargo_output(unrelated, artifact(binary)):
                    self.assertEqual(smoke.find_binary("host"), binary)

    def test_cached_artifact_and_explicit_target(self):
        binary = self.create_binary("cached smoke.exe")
        with self.cargo_output(artifact(binary, fresh=True)) as run:
            self.assertEqual(smoke.find_binary("x86_64-pc-windows-gnu"), binary)
        command = run.call_args.args[0]
        self.assertIn("--no-run", command)
        self.assertIn("--message-format=json-render-diagnostics", command)
        self.assertEqual(command[-2:], ["--target", "x86_64-pc-windows-gnu"])

    def test_missing_or_wrong_artifact_is_rejected(self):
        for message in (
            {"reason": "build-finished", "success": True},
            artifact("ignored", executable=None),
            artifact("ignored", target={"name": "linkme_smoke", "kind": ["lib"]}),
            artifact("ignored", profile={"test": False}),
        ):
            with self.subTest(message=message), self.cargo_output(message):
                with self.assertRaisesRegex(ValueError, "found 0"):
                    smoke.find_binary("host")

    def test_ambiguous_artifacts_are_rejected(self):
        with self.cargo_output(artifact("one"), artifact("two")):
            with self.assertRaisesRegex(ValueError, "found 2"):
                smoke.find_binary("host")

    def test_nonexistent_binary_is_rejected(self):
        with self.cargo_output(artifact(self.root / "absent.exe")):
            with self.assertRaisesRegex(ValueError, "does not exist"):
                smoke.find_binary("host")

    def test_cargo_failure_is_reported_and_preserved(self):
        with patch.object(smoke.subprocess, "run", side_effect=subprocess.CalledProcessError(101, ["cargo"])), \
                contextlib.redirect_stderr(io.StringIO()) as errors:
            self.assertEqual(smoke.main(), 101)
        self.assertIn("Failed to build", errors.getvalue())

    def test_invalid_json_fails_closed(self):
        with patch.object(smoke.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "invalid")):
            with self.assertRaises(ValueError):
                smoke.find_binary("host")


if __name__ == "__main__":
    unittest.main()
