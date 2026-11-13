import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("run_release", Path(__file__).with_name("run-release.py"))
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


class ReleaseRunnerTests(unittest.TestCase):
    def fixture(self, root):
        source = root / "sdk"
        source.mkdir()
        (source / "Makefile").write_text("RUST_IMAGE ?= rust:1.99.1-bookworm@sha256:" + "a" * 64 + "\n")
        archive = root / "debugbundle-1.0.0.crate"
        archive.write_bytes(b"synthetic archive")
        receipts = root / "receipts"
        return ["run-release.py", "preflight", "--source", str(source), "--artifact", str(archive),
                "--receipts", str(receipts)], receipts

    def test_early_container_failure_keeps_its_diagnostics_and_exact_exit_status(self):
        with tempfile.TemporaryDirectory() as directory:
            arguments, receipts = self.fixture(Path(directory))
            result = subprocess.CompletedProcess([], 42, "synthetic Git ownership failure\n")
            with patch("sys.argv", arguments), patch.dict(os.environ, RELEASE_SOURCE_SHA="a" * 40), \
                    patch.object(RUNNER.subprocess, "run", return_value=result) as call, \
                    self.assertRaises(SystemExit) as stopped:
                RUNNER.main()
            self.assertEqual(stopped.exception.code, 42)
            self.assertIn("ownership failure", (receipts / "preflight.log").read_text())
            self.assertEqual(json.loads((receipts / "preflight.json").read_text())["exit_code"], 42)
            command = call.call_args[0][0]
            self.assertIn("/release-candidate.sh", command)
            self.assertTrue(any(value.endswith(":/release-candidate.sh:ro") for value in command))
            self.assertNotIn("CARGO_REGISTRY_TOKEN", command)

    def test_only_publish_forwards_the_standard_cargo_registry_credential(self):
        with tempfile.TemporaryDirectory() as directory:
            arguments, receipts = self.fixture(Path(directory))
            arguments[1] = "publish"
            result = subprocess.CompletedProcess([], 0, "publish completed\n")
            with patch("sys.argv", arguments), patch.dict(os.environ, RELEASE_SOURCE_SHA="a" * 40), \
                    patch.object(RUNNER.subprocess, "run", return_value=result) as call, \
                    self.assertRaises(SystemExit):
                RUNNER.main()
            command = call.call_args[0][0]
            self.assertIn("CARGO_REGISTRY_TOKEN", command)
            self.assertNotIn("CARGO_REGISTRIES_CRATES_IO_TOKEN", command)

    def test_timeouts_retain_partial_output_and_invalid_identity_never_starts_docker(self):
        with tempfile.TemporaryDirectory() as directory:
            arguments, receipts = self.fixture(Path(directory))
            failure = subprocess.TimeoutExpired(["docker"], 1800, output=b"partial compile receipt\n")
            with patch("sys.argv", arguments), patch.dict(os.environ, RELEASE_SOURCE_SHA="a" * 40), \
                    patch.object(RUNNER.subprocess, "run", side_effect=failure), \
                    self.assertRaises(subprocess.TimeoutExpired):
                RUNNER.main()
            self.assertEqual((receipts / "preflight.log").read_text(), "partial compile receipt\n")
            with patch("sys.argv", arguments), patch.dict(os.environ, RELEASE_SOURCE_SHA="bad"), \
                    patch.object(RUNNER.subprocess, "run") as run, self.assertRaises(RuntimeError):
                RUNNER.main()
            run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
