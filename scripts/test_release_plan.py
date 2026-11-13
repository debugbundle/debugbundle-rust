"""Release identity/provenance checks do not need Rust or network."""
import importlib.util
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location("release_plan", Path(__file__).with_name("release-plan.py"))
PLAN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PLAN)


class ReleasePlanTests(unittest.TestCase):
    def run_record(self, **changes):
        value = {"id": 123, "head_sha": "a" * 40, "head_branch": "main", "event": "push",
                 "status": "completed", "conclusion": "success",
                 "head_repository": {"full_name": PLAN.REPOSITORY}}
        value.update(changes)
        return value

    def test_qualification_is_bound_to_the_exact_trusted_source(self):
        self.assertEqual(PLAN.select_run([self.run_record()], "a" * 40)["id"], 123)
        for change in ({"head_sha": "b" * 40}, {"head_branch": "other"},
                       {"event": "pull_request"}, {"head_repository": {"full_name": "other/repo"}}):
            with self.subTest(change=change), self.assertRaises(RuntimeError):
                PLAN.select_run([self.run_record(**change)], "a" * 40)

    def test_newer_failed_or_running_qualification_cannot_use_an_older_success(self):
        for status in ({"status": "completed", "conclusion": "failure"},
                       {"status": "in_progress", "conclusion": None}):
            with self.subTest(status=status), self.assertRaises(RuntimeError):
                PLAN.select_run([self.run_record(), self.run_record(id=124, **status)], "a" * 40)

    def test_every_required_platform_and_boundary_job_must_pass(self):
        jobs = [{"name": name, "conclusion": "success", "status": "completed"}
                for name in PLAN.REQUIRED_JOBS]
        PLAN.check_jobs(jobs)
        for bad in (jobs[:-1], jobs + [jobs[0]],
                    [{**jobs[0], "conclusion": "failure"}] + jobs[1:]):
            with self.assertRaises(RuntimeError):
                PLAN.check_jobs(bad)

    def test_registry_state_selects_bootstrap_trusted_or_exact_verification_only(self):
        digest = "a" * 64
        self.assertEqual(PLAN.registry_state(None, None, "1.0.0", digest), ("bootstrap", False))
        self.assertEqual(PLAN.registry_state({}, None, "1.0.0", digest), ("trusted", False))
        entry = {"name": "debugbundle", "vers": "1.0.0", "cksum": digest, "yanked": False}
        self.assertEqual(PLAN.registry_state({}, [entry], "1.0.0", digest), ("none", True))
        for changed in ({"cksum": "b" * 64}, {"yanked": True}):
            with self.subTest(changed=changed), self.assertRaises(RuntimeError):
                PLAN.registry_state({}, [{**entry, **changed}], "1.0.0", digest)

    def test_ref_and_version_validation_rejects_shell_or_ambiguous_inputs(self):
        for value in ("main", "refs/heads/main", "v1.0.0-alpha.1", "refs/tags/v1.0.0", "a" * 40):
            PLAN.valid_ref(value)
        for value in ("other", "--help", "main\n", "v1.0.0;echo x", "$(id)", "a" * 39):
            with self.subTest(value=value), self.assertRaises(RuntimeError):
                PLAN.valid_ref(value)

    def test_archive_identity_cannot_substitute_source_version_or_dirty_bytes(self):
        vcs = {"git": {"sha1": "a" * 40, "dirty": False}}
        package = {"name": "debugbundle", "version": "1.0.0"}
        self.assertEqual(PLAN.check_metadata(vcs, package, "a" * 40,
                                            "debugbundle-1.0.0.crate", "v1.0.0"), "1.0.0")
        cases = [({"git": {"sha1": "b" * 40}}, package, "debugbundle-1.0.0.crate", "main"),
                 ({"git": {"sha1": "a" * 40, "dirty": True}}, package, "debugbundle-1.0.0.crate", "main"),
                 (vcs, {**package, "name": "other"}, "debugbundle-1.0.0.crate", "main"),
                 (vcs, package, "debugbundle-2.0.0.crate", "main"),
                 (vcs, package, "debugbundle-1.0.0.crate", "v2.0.0")]
        for state, metadata, filename, ref in cases:
            with self.subTest(filename=filename, ref=ref), self.assertRaises(RuntimeError):
                PLAN.check_metadata(state, metadata, "a" * 40, filename, ref)


if __name__ == "__main__":
    unittest.main()
