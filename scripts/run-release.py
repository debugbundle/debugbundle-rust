#!/usr/bin/env python3
"""Run current release tooling against a separately qualified, unchanged source."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=["preflight", "publish", "verify"])
    parser.add_argument("--source", required=True, type=Path)
    parser.add_argument("--artifact", required=True, type=Path)
    parser.add_argument("--receipts", required=True, type=Path)
    args = parser.parse_args()
    source, artifact = args.source.resolve(), args.artifact.resolve()
    args.receipts.mkdir(parents=True, exist_ok=True)
    try:
        if not re.fullmatch(r"[0-9a-f]{40}", os.environ.get("RELEASE_SOURCE_SHA", "")):
            raise RuntimeError("Missing immutable source identity")
        if args.mode == "verify":
            command = ["make", "smoke-published"]
        else:
            makefile = (source / "Makefile").read_text()
            match = re.search(r"^RUST_IMAGE \?= (rust:[0-9.]+-bookworm@sha256:[0-9a-f]{64})$", makefile, re.M)
            if not match:
                raise RuntimeError("Source does not select a pinned Rust container")
            script = Path(__file__).with_name("release-candidate.sh").resolve()
            command = ["docker", "run", "--rm", "--init", "--ulimit", "core=0:0",
                       "-v", f"{source}:/work", "-v", f"{artifact}:/selected/{artifact.name}:ro",
                       "-v", f"{script}:/release-candidate.sh:ro", "-w", "/work",
                       "-v", "debugbundle-rust-registry:/usr/local/cargo/registry",
                       "-v", "debugbundle-rust-target:/target", "-e", "CARGO_TARGET_DIR=/target",
                       "-e", "CARGO_INCREMENTAL=0", "-e", "RELEASE_VERSION", "-e", "RELEASE_SOURCE_SHA",
                       "-e", "DEBUGBUNDLE_CANDIDATE_SHA256",
                       "-e", f"DEBUGBUNDLE_CANDIDATE_ARTIFACT=/selected/{artifact.name}"]
            if args.mode == "publish":
                command += ["-e", "CARGO_REGISTRIES_CRATES_IO_TOKEN"]
            command += [match[1], "sh", "/release-candidate.sh", args.mode]
        result = subprocess.run(command, cwd=source, stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, text=True, timeout=1800)
        (args.receipts / f"{args.mode}.log").write_text(result.stdout)
        print(result.stdout, end="", flush=True)
        (args.receipts / f"{args.mode}.json").write_text(json.dumps({"mode": args.mode,
            "source_sha": os.environ["RELEASE_SOURCE_SHA"], "exit_code": result.returncode}, indent=2) + "\n")
        raise SystemExit(result.returncode)
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        if isinstance(error, subprocess.TimeoutExpired) and error.output:
            output = error.output.decode(errors="replace") if isinstance(error.output, bytes) else error.output
            (args.receipts / f"{args.mode}.log").write_text(output)
        (args.receipts / f"{args.mode}-error.txt").write_text(str(error) + "\n")
        raise


if __name__ == "__main__":
    main()
