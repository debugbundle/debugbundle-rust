"""Exercise the shell preflight with isolated Cargo output and stale caches."""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class CandidatePreflightTests(unittest.TestCase):
    def run_preflight(self, published_bytes):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            for name in ("release-candidate.sh", "check-candidate.sh"):
                shutil.copy(Path(__file__).with_name(name), root / "scripts" / name)
            tools = root / "bin"
            tools.mkdir()
            (tools / "git").write_text("#!/bin/sh\ncase \"$*\" in *rev-parse*) printf '%s\\n' \"$RELEASE_SOURCE_SHA\";; *status*) :;; esac\n")
            (tools / "cargo").write_text(
                "#!/bin/sh\ncase \"$1\" in\n"
                "pkgid) echo debugbundle@1.0.0-alpha.1;;\n"
                "publish) mkdir -p \"$CARGO_TARGET_DIR/package/tmp-crate\"; "
                "printf '%s' \"$PUBLISH_BYTES\" > \"$CARGO_TARGET_DIR/package/tmp-crate/debugbundle-1.0.0-alpha.1.crate\";;\nesac\n")
            for tool in tools.iterdir():
                tool.chmod(0o755)
            archive = root / "debugbundle-1.0.0-alpha.1.crate"
            archive.write_bytes(b"qualified bytes")
            # This stale archive made the earlier local preflight falsely pass.
            cached = root / "target/package"
            cached.mkdir(parents=True)
            shutil.copy(archive, cached / archive.name)
            environment = {**os.environ, "PATH": str(tools) + os.pathsep + os.environ["PATH"],
                           "CARGO_TARGET_DIR": str(root / "target"),
                           "RELEASE_SOURCE_SHA": "a" * 40, "RELEASE_VERSION": "1.0.0-alpha.1",
                           "DEBUGBUNDLE_CANDIDATE_ARTIFACT": str(archive),
                           "DEBUGBUNDLE_CANDIDATE_SHA256": hashlib.sha256(archive.read_bytes()).hexdigest(),
                           "PUBLISH_BYTES": published_bytes}
            result = subprocess.run(["sh", "scripts/release-candidate.sh", "preflight"],
                                    cwd=root, env=environment, capture_output=True, text=True)
            marker = (root / ".tmp/release/preflight.sha256").exists()
            for package_target in (root / ".tmp/release").glob("preflight-package.*"):
                self.assertEqual(package_target.stat().st_mode & 0o777, 0o755)
            return result, marker

    def test_fresh_publish_output_passes_and_stale_package_cannot_mask_a_mismatch(self):
        good, marker = self.run_preflight("qualified bytes")
        self.assertEqual(good.returncode, 0, good.stdout + good.stderr)
        self.assertTrue(marker)
        bad, marker = self.run_preflight("different publish bytes")
        self.assertNotEqual(bad.returncode, 0)
        self.assertIn("Candidate checksum mismatch", bad.stderr)
        self.assertFalse(marker)


if __name__ == "__main__":
    unittest.main()
