#!/usr/bin/env python3
"""Native CI only: preserve platform semantics without a Docker Linux surrogate."""
import argparse
import os
from pathlib import Path
import subprocess
import tempfile


def check_coverage(root, receipt, prefix):
    expected = {
        path.relative_to(root).as_posix()
        for path in (root / "src").rglob("*.rs")
        if path.name != "tests.rs" and "tests" not in path.relative_to(root).parts
    }
    if os.name == "nt":
        expected.discard("src/storage/unix.rs")
    else:
        expected = {path for path in expected if path != "src/storage/windows.rs" and not path.startswith("src/storage/windows/")}
    prefix = prefix.replace("\\", "/").rstrip("/") + "/"
    observed = {}
    source = None
    lines = hits = 0
    for line in receipt.read_text().splitlines():
        if line.startswith("SF:"):
            value = line[3:].replace("\\", "/")
            source = value[len(prefix):] if value.lower().startswith(prefix.lower()) else None
            lines = hits = 0
        elif line.startswith("LF:"):
            lines = int(line[3:])
        elif line.startswith("LH:"):
            hits = int(line[3:])
        elif line == "end_of_record" and source in expected:
            if source in observed:
                raise RuntimeError(f"duplicate production coverage: {source}")
            observed[source] = (hits, lines)
    missing = expected - observed.keys()
    failed = [f"{path}: {hits}/{lines}" for path, (hits, lines) in observed.items() if lines <= 0 or hits / lines < 0.8]
    if missing or failed:
        raise RuntimeError(f"missing coverage: {sorted(missing)}; below 80 percent: {failed}")
    print(f"Native per-file executable-line coverage passed for {len(expected)} modules")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("target", choices=["typecheck", "test", "test-http", "coverage", "coverage-check"])
    parser.add_argument("--lcov", default=".tmp/coverage/native.info")
    parser.add_argument("--receipt-prefix")
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    if args.target == "coverage-check":
        check_coverage(root, root / args.lcov, args.receipt_prefix or str(root))
        return
    command = {
        "typecheck": ["cargo", "check", "--all-targets", "--all-features", "--locked"],
        "test": ["cargo", "test", "--all-features", "--locked"],
        "test-http": ["cargo", "test", "--locked"],
        "coverage": ["cargo", "llvm-cov", "--all-features", "--locked", "--lcov", "--output-path", args.lcov,
                     "--ignore-filename-regex", r"[/\\]tests[/\\]|[/\\]tests\.rs$|[/\\]examples[/\\]"],
    }[args.target]
    if args.target == "coverage":
        (root / args.lcov).parent.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, CARGO_INCREMENTAL="0", CARGO_BUILD_JOBS="1", RUST_TEST_THREADS="1")
    # macOS commonly exposes /var as a symlink. Put synthetic fixtures under the
    # physical OS scratch root; never weaken the SDK's no-follow path boundary.
    scratch = str(Path(tempfile.gettempdir()).resolve())
    env.update(TMPDIR=scratch, TMP=scratch, TEMP=scratch)
    subprocess.run(command, cwd=root, env=env, check=True, timeout=1800)
    if args.target == "coverage":
        check_coverage(root, root / args.lcov, str(root))


if __name__ == "__main__":
    main()
