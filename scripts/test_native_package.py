"""Adverse artifact fixtures; no host Rust or network is used."""
import hashlib
import gzip
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("native_package", Path(__file__).with_name("native-package.py"))
NATIVE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(NATIVE)
ROOT = "debugbundle-1.0.0-alpha.1"
REQUIRED = ["Cargo.toml", "Cargo.lock", "LICENSE", "README.md", "docs/runtime-and-api.md"]


class NativeArtifactTests(unittest.TestCase):
    def artifact(self, directory, extra=()):
        artifact = directory / (ROOT + ".crate")
        with tarfile.open(artifact, "w:gz") as archive:
            for name in REQUIRED:
                member = tarfile.TarInfo(ROOT + "/" + name)
                member.size = 2
                archive.addfile(member, io.BytesIO(b"ok"))
            for member in extra:
                archive.addfile(member, io.BytesIO(b"x" * member.size) if member.isfile() else None)
        return artifact

    def test_exact_digest_is_required_when_supplied(self):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            artifact = self.artifact(directory)
            digest = hashlib.sha256(artifact.read_bytes()).hexdigest()
            self.assertEqual(NATIVE.verify_digest(artifact, digest), digest)
            for invalid in ("0" * 64, digest.upper(), "", digest + "\n", "../artifact"):
                with self.subTest(expected=invalid), self.assertRaises(RuntimeError):
                    NATIVE.verify_digest(artifact, invalid)

    def test_prepare_only_needs_no_writable_repository_output_or_host_rust(self):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            artifact = self.artifact(directory)
            repository = directory / "repository"
            (repository / "scripts").mkdir(parents=True)
            # A file makes output creation fail on every platform, even when
            # tests run as root. Validation should write only owned OS scratch.
            (repository / ".tmp").write_text("untouched")
            argv = ["native-package.py", "--prepare-only", "--artifact", str(artifact),
                    "--expected-sha256", NATIVE.verify_digest(artifact)]
            with patch.object(NATIVE, "__file__", str(repository / "scripts/native-package.py")), \
                    patch("sys.argv", argv), patch.object(NATIVE, "run", side_effect=AssertionError("host Rust invoked")):
                NATIVE.main()
            self.assertEqual((repository / ".tmp").read_text(), "untouched")

    def test_owned_regular_archive_extracts(self):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            paths = ("src/lib.rs", "src/storage/mod.rs", "tests/helpers/mod.rs",
                     "tests/fixtures/privacy-conformance.json", "tests/fixtures/privacy-conformance.sha256",
                     "examples/local.rs", "examples/README.md", "docs/guides/nested.md")
            members = [tarfile.TarInfo(ROOT + "/" + path) for path in paths]
            extracted = NATIVE.prepare(self.artifact(directory, members), directory / "output")
            self.assertEqual(extracted, directory / "output" / ROOT)
            self.assertEqual((extracted / "README.md").read_bytes(), b"ok")
            self.assertTrue(all((extracted / path).is_file() for path in paths))

    def test_development_files_are_rejected_before_copy(self):
        for path in ("smoke/browser-artifacts/node_modules/example/README.md",
                     "smoke/browser-artifacts/node_modules/example/LICENSE",
                     "src/.DS_Store", "src/.private.rs", "src/token.pem",
                     "src/node_modules/generated.rs", "docs/.cache/README.md"):
            with self.subTest(path=path), tempfile.TemporaryDirectory() as directory:
                directory = Path(directory)
                member = tarfile.TarInfo(ROOT + "/" + path)
                with self.assertRaisesRegex(RuntimeError, "unexpected package member"):
                    NATIVE.prepare(self.artifact(directory, [member]), directory / "output")
                self.assertFalse((directory / "output").exists())

    def test_negative_member_lengths_are_rejected_before_advancing_the_reader(self):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            member = tarfile.TarInfo(ROOT + "/invalid")
            member.size = -512
            artifact = directory / (ROOT + ".crate")
            artifact.write_bytes(gzip.compress(member.tobuf(format=tarfile.GNU_FORMAT) + b"\0" * 1024))
            # Newer Python readers reject this header before prepare() receives
            # its first member. Both paths must fail before creating output.
            with self.assertRaises((RuntimeError, tarfile.TarError)):
                NATIVE.prepare(artifact, directory / "output")
            self.assertFalse((directory / "output").exists())

    def test_paths_and_special_members_are_rejected_before_copy(self):
        paths = ("../escape", "/escape", ROOT + "/../escape", ROOT + "/a\\b", ROOT + "/a:b",
                 ROOT + "/a\x01b", ROOT + "/file.", ROOT + "/dir /file", ROOT + "/CON.txt",
                 ROOT + "/lpt1", ROOT + "/COM\u00b9.txt", ROOT + "/./file")
        for name in paths:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                directory = Path(directory)
                artifact = self.artifact(directory, [tarfile.TarInfo(name)])
                with self.assertRaises(RuntimeError):
                    NATIVE.prepare(artifact, directory / "output")
                self.assertFalse((directory / "output").exists())
        for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.FIFOTYPE, tarfile.CHRTYPE):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as directory:
                directory = Path(directory)
                member = tarfile.TarInfo(ROOT + "/special")
                member.type, member.linkname = kind, "../../escape"
                with self.assertRaises(RuntimeError):
                    NATIVE.prepare(self.artifact(directory, [member]), directory / "output")
                self.assertFalse((directory / "output").exists())

    def test_duplicate_and_portable_case_aliases_are_rejected_before_copy(self):
        for names in (("README.md",), ("readme.md",), ("other", "other/child"), ("unicode/e\u0301", "unicode/\u00e9")):
            with self.subTest(names=names), tempfile.TemporaryDirectory() as directory:
                directory = Path(directory)
                members = [tarfile.TarInfo(ROOT + "/" + name) for name in names]
                with self.assertRaises(RuntimeError):
                    NATIVE.prepare(self.artifact(directory, members), directory / "output")
                self.assertFalse((directory / "output").exists())


if __name__ == "__main__":
    unittest.main()
