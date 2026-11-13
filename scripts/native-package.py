"""Native CI artifact consumers; extraction checks can also run without host Rust."""
import argparse
import hashlib
import gzip
import io
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unicodedata


def run(command, cwd, env, timeout=600):
    return subprocess.run(command, cwd=cwd, env=env, check=True, text=True,
                          stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=timeout).stdout


def verify_digest(artifact, expected=None):
    if artifact.stat().st_size > 8 * 1024 * 1024:
        raise RuntimeError("oversized candidate")
    actual = hashlib.sha256(artifact.read_bytes()).hexdigest()
    if expected is not None and (re.fullmatch(r"[0-9a-f]{64}", expected) is None or actual != expected):
        raise RuntimeError("candidate checksum mismatch")
    return actual


def prepare(artifact, destination):
    if artifact.stat().st_size > 8 * 1024 * 1024:
        raise RuntimeError("oversized candidate")
    with gzip.open(artifact, "rb") as compressed:
        body = compressed.read(20 * 1024 * 1024 + 1)
    if len(body) > 20 * 1024 * 1024:
        raise RuntimeError("oversized decompressed candidate")
    with tarfile.open(fileobj=io.BytesIO(body), mode="r:") as archive:
        members = []
        payload_bytes = 0
        for member in archive:
            # Validate before advancing to the next header: malformed negative
            # lengths can otherwise move a tar reader back onto an earlier one.
            if member.size < 0 or len(members) >= 1000 or member.size > 16 * 1024 * 1024 - payload_bytes:
                raise RuntimeError("invalid candidate member budget")
            payload_bytes += member.size
            members.append(member)
        roots = {member.name.split("/", 1)[0] for member in members}
        if roots != {artifact.stem} or not re.fullmatch(r"debugbundle-[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?", artifact.stem):
            raise RuntimeError("invalid candidate contents")
        paths = {}
        for member in members:
            parts = member.name.rstrip("/").split("/")
            if not (member.isfile() or member.isdir()) or "\\" in member.name or any(part in ("..", ".", "") or any(ord(char) < 32 or ord(char) == 127 or 0xd800 <= ord(char) <= 0xdfff or char in ':<>|?*' for char in part) or part.rstrip(" .") != part for part in parts):
                raise RuntimeError("unsafe candidate path")
            for part in parts:
                stem = part.split(".", 1)[0].upper()
                if stem in {"CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM¹", "COM²", "COM³", "LPT¹", "LPT²", "LPT³"} or (len(stem) == 4 and stem[:3] in {"COM", "LPT"} and stem[3].isdigit()):
                    raise RuntimeError("nonportable candidate device name")
            portable = tuple(unicodedata.normalize("NFD", part).casefold() for part in parts)
            if portable in paths:
                raise RuntimeError("duplicate candidate path or platform alias")
            paths[portable] = member.isdir()
        for path in paths:
            if any(paths.get(path[:length]) is False for length in range(1, len(path))):
                raise RuntimeError("candidate file is an ancestor of another member")
        root_files = {"Cargo.toml", "Cargo.toml.orig", "Cargo.lock", ".cargo_vcs_info.json",
                      "build.rs", "LICENSE", "README.md", "CHANGELOG.md"}
        for member in members:
            relative = member.name.rstrip("/").split("/")[1:]
            name = "/".join(relative)
            if name in root_files and member.isfile():
                continue
            if any(part.startswith(".") or part in {"node_modules", "target"} for part in relative):
                raise RuntimeError(f"unexpected package member: {member.name}")
            allowed = (not relative or relative[0] in {"src", "tests", "examples", "docs"}) if member.isdir() else bool(
                re.fullmatch(r"(?:src|tests|examples)/.+\.rs|tests/fixtures/[^/]+\.(?:json|sha256)|examples/README\.md|docs/.+\.md", name))
            if not allowed:
                raise RuntimeError(f"unexpected package member: {member.name}")
        # Copy only validated regular files/directories into a new owned temp
        # root. Do not restore archive permissions, links, devices or owner data.
        for member in members:
            target = destination.joinpath(*member.name.rstrip("/").split("/"))
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                source = archive.extractfile(member)
                if source is None:
                    raise RuntimeError("missing candidate file body")
                with source, target.open("xb") as stream:
                    shutil.copyfileobj(source, stream)
    extracted = destination / next(iter(roots))
    for file in ("Cargo.toml", "Cargo.lock", "LICENSE", "README.md", "docs/runtime-and-api.md"):
        if not (extracted / file).is_file():
            raise RuntimeError(f"missing artifact file: {file}")
    return extracted


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifact", type=Path)
    parser.add_argument("--expected-sha256")
    parser.add_argument("--prepare-only", action="store_true")
    args = parser.parse_args()
    if args.expected_sha256 is not None and args.artifact is None:
        parser.error("--expected-sha256 requires --artifact")
    root = Path(__file__).resolve().parent.parent
    env = dict(os.environ, CARGO_INCREMENTAL="0", CARGO_BUILD_JOBS="1", RUST_TEST_THREADS="1")
    scratch = str(Path(tempfile.gettempdir()).resolve())
    env.update(TMPDIR=scratch, TMP=scratch, TEMP=scratch)
    output = root / ".tmp/native-package"
    if not args.prepare_only:
        output.mkdir(parents=True, exist_ok=True)
    artifact = args.artifact
    if artifact is None:
        if args.prepare_only:
            parser.error("--prepare-only requires --artifact")
        metadata = json.loads(run(["cargo", "metadata", "--no-deps", "--locked", "--format-version", "1"], root, env))
        package = next(package for package in metadata["packages"] if package["name"] == "debugbundle")
        (output / "package.log").write_text(run(["cargo", "package", "--locked", "--allow-dirty"], root, env))
        artifact = Path(metadata["target_directory"]) / "package" / f'debugbundle-{package["version"]}.crate'
    artifact = artifact.resolve()
    digest = verify_digest(artifact, args.expected_sha256)
    with tempfile.TemporaryDirectory(prefix="debugbundle-native-package-") as directory:
        directory = Path(directory)
        extracted = prepare(artifact, directory / "artifact")
        if args.prepare_only:
            print("Native artifact extraction/path/contents validation passed; no native Rust runtime claim")
            return
        consumers = directory / "consumers"
        shutil.copytree(root / "smoke/package-consumers", consumers)
        manifest = (consumers / "Cargo.toml.in").read_text()
        # JSON quoted strings are also valid TOML basic strings; preserve native
        # path characters without injecting manifest syntax.
        manifest = manifest.replace('"__SDK_PATH__"', json.dumps(extracted.as_posix()))
        (consumers / "Cargo.toml").write_text(manifest)
        shutil.copyfile(root / "tests/fixtures/relay-compliance.json", consumers / "support/relay-compliance.json")
        env["DEBUGBUNDLE_ARTIFACT_EVENTS_OUTPUT"] = str(output / "rust-events.json")
        run(["cargo", "fetch", "--locked"], consumers, env)
        run(["cargo", "fetch", "--manifest-path", str(extracted / "Cargo.toml"), "--locked"], consumers, env)
        metadata_text = run(["cargo", "metadata", "--locked", "--offline", "--format-version", "1"], consumers, env)
        metadata = json.loads(metadata_text)
        sdks = [package for package in metadata["packages"] if package["name"] == "debugbundle"]
        if len(sdks) != 1 or sdks[0]["source"] is not None or Path(sdks[0]["manifest_path"]).resolve() != (extracted / "Cargo.toml").resolve():
            raise RuntimeError("native consumer resolves back to checkout or another SDK")
        (output / "consumer-metadata.json").write_text(metadata_text)
        (output / "packaged-tests.log").write_text(run(["cargo", "test", "--manifest-path", str(extracted / "Cargo.toml"), "--all-features", "--locked", "--offline"], consumers, env, 1200))
        for package in ("artifact-vanilla", "artifact-axum", "artifact-actix"):
            (output / f"{package}.log").write_text(run(["cargo", "run", "--locked", "--offline", "-p", package], consumers, env))
        receipt = {"artifact_sha256": digest,
                   "sdk_lock_sha256": hashlib.sha256((extracted / "Cargo.lock").read_bytes()).hexdigest(),
                   "consumer_lock_sha256": hashlib.sha256((consumers / "Cargo.lock").read_bytes()).hexdigest(),
                   "rustc": run(["rustc", "-vV"], root, env), "platform": os.name}
        (output / "receipt.json").write_text(json.dumps(receipt, indent=2))
    print("Native extracted-crate tests and vanilla/Axum/non-Send Actix consumers passed")


if __name__ == "__main__":
    main()
