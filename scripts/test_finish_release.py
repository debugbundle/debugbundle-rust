import importlib.util
import os
from pathlib import Path
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("finish_release", Path(__file__).with_name("finish-release.py"))
FINISH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FINISH)


class ReleaseTagTests(unittest.TestCase):
    def test_existing_tag_for_different_source_never_creates_or_changes_a_release(self):
        environment = {"GITHUB_REPOSITORY": "debugbundle/debugbundle-rust",
                       "RELEASE_VERSION": "1.0.0", "RELEASE_SOURCE_SHA": "a" * 40}
        with patch.dict(os.environ, environment), patch.object(FINISH, "tag_source", return_value="b" * 40), \
                patch.object(FINISH.subprocess, "run") as run, self.assertRaises(RuntimeError):
            FINISH.main()
        run.assert_not_called()

    def test_tag_identity_supports_lightweight_and_annotated_tags(self):
        with patch.object(FINISH, "api", return_value={"object": {"type": "commit", "sha": "a" * 40}}):
            self.assertEqual(FINISH.tag_source("owner/repo", "v1.0.0"), "a" * 40)
        with patch.object(FINISH, "api", side_effect=[{"object": {"type": "tag", "sha": "a" * 40}},
                                                     {"object": {"type": "commit", "sha": "b" * 40}}]):
            self.assertEqual(FINISH.tag_source("owner/repo", "v1.0.0"), "b" * 40)

    def test_unknown_or_cyclic_tag_targets_fail_closed(self):
        for target in ({"type": "tree", "sha": "a" * 40}, {"type": "commit", "sha": "bad"},
                       {"type": "tag", "sha": "a" * 40}):
            with self.subTest(target=target), patch.object(FINISH, "api", return_value={"object": target}), \
                    self.assertRaises(RuntimeError):
                FINISH.tag_source("owner/repo", "v1.0.0")


if __name__ == "__main__":
    unittest.main()
