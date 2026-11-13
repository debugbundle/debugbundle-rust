#!/usr/bin/env python3
"""Select an immutable, already-qualified source and archive; never publish."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

REPOSITORY = "debugbundle/debugbundle-rust"
VERSION = r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?"
REQUIRED_JOBS = {
    "candidate", "coverage-security", "bounded-fuzz", "browser-resources",
    "linux (ubuntu-24.04)", "linux (ubuntu-24.04-arm)",
    "native (macos-15, test, python3)", "native (windows-2025, test, python)",
}


def valid_ref(value):
    if not (value in {"main", "refs/heads/main"} or re.fullmatch(r"[0-9a-f]{40}", value)
            or re.fullmatch(r"(?:refs/tags/)?v" + VERSION, value)):
        raise RuntimeError("Release ref must be main, an exact commit or a version tag")


def select_run(runs, source):
    trusted = [run for run in runs if run["head_sha"] == source and run["head_branch"] == "main"
               and run["event"] in {"push", "workflow_dispatch"}
               and run["head_repository"]["full_name"] == REPOSITORY]
    if not trusted:
        raise RuntimeError("No qualification exists for this exact trusted source")
    selected = max(trusted, key=lambda run: run["id"])
    if selected["status"] != "completed" or selected["conclusion"] != "success":
        raise RuntimeError("Latest qualification for this source has not passed")
    return selected


def check_jobs(jobs):
    if len(jobs) != len(REQUIRED_JOBS) or {job["name"] for job in jobs} != REQUIRED_JOBS:
        raise RuntimeError("Qualification does not contain the complete eight-job matrix")
    if any(job["status"] != "completed" or job["conclusion"] != "success" for job in jobs):
        raise RuntimeError("Every qualification job must have passed")


def registry_state(crate, entries, version, digest):
    if crate is None:
        return "bootstrap", False
    matches = [entry for entry in (entries or []) if entry.get("name") == "debugbundle"
               and entry.get("vers") == version]
    if not matches:
        return "trusted", False
    if len(matches) != 1 or matches[0].get("yanked") is not False or matches[0].get("cksum") != digest:
        raise RuntimeError("Existing registry version is yanked, ambiguous or has different bytes")
    return "none", True


def check_metadata(vcs, package, source, filename, ref):
    version = package.get("version", "")
    if package.get("name") != "debugbundle" or not re.fullmatch(VERSION, version):
        raise RuntimeError("Invalid qualified package identity")
    if vcs.get("git", {}).get("sha1") != source or vcs["git"].get("dirty", False):
        raise RuntimeError("Archive does not contain the exact clean qualified source")
    if filename != f"debugbundle-{version}.crate":
        raise RuntimeError("Artifact filename differs from package version")
    if ref.startswith(("v", "refs/tags/v")) and ref.rsplit("/", 1)[-1] != "v" + version:
        raise RuntimeError("Version tag differs from the qualified package")
    return version


def gh(path):
    return json.loads(subprocess.check_output(["gh", "api", path], text=True))


def public_read(url):
    request = urllib.request.Request(url, headers={"User-Agent":
        "debugbundle-rust-release/1.0 (https://github.com/debugbundle/debugbundle-rust)"})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            data = response.read(8 * 1024 * 1024 + 1)
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return None
        raise
    if len(data) > 8 * 1024 * 1024:
        raise RuntimeError("Registry response exceeds limit")
    return data


def resolve(ref, directory, wait):
    import tomllib  # Release runners provide Python 3.11+; pure checks support host Python 3.9.
    valid_ref(ref)
    source = subprocess.check_output(
        ["git", "rev-parse", "--verify", "--end-of-options", ref + "^{commit}"], text=True).strip()
    if not re.fullmatch(r"[0-9a-f]{40}", source):
        raise RuntimeError("Invalid source identity")
    subprocess.run(["git", "merge-base", "--is-ancestor", source, "origin/main"], check=True)
    deadline = time.monotonic() + wait
    while True:
        runs = gh(f"repos/{REPOSITORY}/actions/workflows/ci.yml/runs?head_sha={source}&per_page=30")
        try:
            run = select_run(runs["workflow_runs"], source)
            break
        except RuntimeError:
            active = any(item["head_sha"] == source and item["status"] != "completed"
                         for item in runs["workflow_runs"])
            if not active or time.monotonic() >= deadline:
                raise
            print("Waiting for existing qualification; no duplicate matrix is started", flush=True)
            time.sleep(20)
    jobs = gh(f"repos/{REPOSITORY}/actions/runs/{run['id']}/jobs?per_page=100")["jobs"]
    check_jobs(jobs)
    candidate = directory / "candidate"
    candidate.mkdir(parents=True, exist_ok=False)
    subprocess.run(["gh", "run", "download", str(run["id"]), "--repo", REPOSITORY,
                    "--name", "rust-candidate", "--dir", str(candidate)], check=True)
    archives = list(candidate.glob("*.crate"))
    if len(archives) != 1:
        raise RuntimeError("Expected exactly one qualified crate")
    artifact = archives[0]
    digest = hashlib.sha256(artifact.read_bytes()).hexdigest()
    spec = importlib.util.spec_from_file_location("native_package", Path(__file__).with_name("native-package.py"))
    native = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(native)
    with tempfile.TemporaryDirectory() as scratch:
        extracted = native.prepare(artifact, Path(scratch) / "extracted")
        vcs = json.loads((extracted / ".cargo_vcs_info.json").read_text())
        package = tomllib.loads((extracted / "Cargo.toml").read_text())["package"]
        version = check_metadata(vcs, package, source, artifact.name, ref)
    checksums = (candidate / "checksums.txt").read_text().splitlines()
    if not any(line.split() == [digest, f"/target/package/{artifact.name}"] for line in checksums):
        raise RuntimeError("Qualified checksum receipt does not match the archive")
    crate = public_read("https://crates.io/api/v1/crates/debugbundle")
    index = public_read("https://index.crates.io/de/bu/debugbundle") if crate is not None else None
    entries = [json.loads(line) for line in index.decode().splitlines() if line] if index else None
    authentication, published = registry_state(crate, entries, version, digest)
    return {"source_sha": source, "version": version, "sha256": digest,
            "qualification_run": str(run["id"]), "authentication": authentication,
            "already_published": str(published).lower(), "artifact": str(artifact),
            "qualification_url": run["html_url"]}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ref", required=True)
    parser.add_argument("--output", type=Path, default=Path(".tmp/release"))
    parser.add_argument("--wait-seconds", type=int, default=0)
    args = parser.parse_args()
    if os.environ.get("GITHUB_REPOSITORY", REPOSITORY) != REPOSITORY:
        raise RuntimeError("Wrong publishing repository")
    args.output.mkdir(parents=True, exist_ok=True)
    result = resolve(args.ref, args.output.resolve(), args.wait_seconds)
    (args.output / "plan.json").write_text(json.dumps(result, indent=2) + "\n")
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            for key in ["source_sha", "version", "sha256", "qualification_run", "authentication", "already_published"]:
                output.write(f"{key}={result[key]}\n")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
