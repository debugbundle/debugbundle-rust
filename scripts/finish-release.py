#!/usr/bin/env python3
"""Create the normal versioned GitHub release only after registry acceptance."""
import json
import os
from pathlib import Path
import re
import subprocess


def api(path, missing=False):
    result = subprocess.run(["gh", "api", path], text=True, capture_output=True)
    if result.returncode:
        if missing and "(HTTP 404)" in result.stderr:
            return None
        raise RuntimeError(result.stderr)
    return json.loads(result.stdout)


def tag_source(repository, tag):
    value = api(f"repos/{repository}/git/ref/tags/{tag}", missing=True)
    if value is None:
        return None
    target = value["object"]
    for _ in range(4):
        if not re.fullmatch(r"[0-9a-f]{40}", target["sha"]):
            raise RuntimeError("Invalid tag target")
        if target["type"] == "commit":
            return target["sha"]
        if target["type"] != "tag":
            break
        target = api(f"repos/{repository}/git/tags/{target['sha']}")["object"]
    raise RuntimeError("Unresolved or unsupported release tag")


def main():
    repository = os.environ["GITHUB_REPOSITORY"]
    version, source = os.environ["RELEASE_VERSION"], os.environ["RELEASE_SOURCE_SHA"]
    if repository != "debugbundle/debugbundle-rust" or not re.fullmatch(
            r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", version) or not re.fullmatch(r"[0-9a-f]{40}", source):
        raise RuntimeError("Invalid release identity")
    tag = "v" + version
    existing_source = tag_source(repository, tag)
    if existing_source is not None and existing_source != source:
        raise RuntimeError("Existing version tag points at different source; never retag")
    artifact = Path(os.environ["RELEASE_ARTIFACT"])
    existing = api(f"repos/{repository}/releases/tags/{tag}", missing=True)
    if existing is None:
        command = ["gh", "release", "create", tag, "--repo", repository, "--target", source,
                   "--generate-notes", str(artifact)]
        if "-" in version:
            command += ["--prerelease"]
        subprocess.run(command, check=True)
    else:
        if existing_source != source or existing.get("draft", True):
            raise RuntimeError("Existing GitHub release is not the accepted published source")
        # A retry after an asset-upload failure may finish this non-registry step.
        if not any(asset["name"] == artifact.name for asset in existing.get("assets", [])):
            subprocess.run(["gh", "release", "upload", tag, str(artifact), "--repo", repository], check=True)
        print("Existing versioned GitHub release retained; registry upload is never repeated")


if __name__ == "__main__":
    main()
