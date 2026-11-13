#!/bin/sh
set -eu
mode=${1:-run}
case "$mode" in build|run) ;; *) exit 2 ;; esac
mkdir -p .tmp/fuzz
sha256sum fuzz/Cargo.lock > .tmp/fuzz/lock.sha256
rustc +nightly-2026-11-12 -vV > .tmp/fuzz/toolchain.txt
cargo +nightly-2026-11-12 fuzz --version >> .tmp/fuzz/toolchain.txt
if [ "$mode" = build ]; then
    timeout 1200 cargo +nightly-2026-11-12 fuzz build --fuzz-dir fuzz --target-dir /target/fuzz --codegen-units 16 --no-cfg-fuzzing
    sha256sum -c .tmp/fuzz/lock.sha256
    exit 0
fi
for target in wire snapshots relay files; do
    # Reproducible mutation seed; independently bounded executions/time/body/RSS.
    # One static SDK runtime remains live, so process-exit leak detection is not
    # a valid leak oracle here. Ownership/cardinality have separate assertions.
    if timeout 90 cargo +nightly-2026-11-12 fuzz run "$target" --fuzz-dir fuzz --target-dir /target/fuzz --codegen-units 16 --no-cfg-fuzzing -- -runs=4096 -max_total_time=30 -max_len=65536 -timeout=10 -rss_limit_mb=768 -seed=212864 -detect_leaks=0 > ".tmp/fuzz/$target.log" 2>&1; then
        tail -n 4 ".tmp/fuzz/$target.log"
    else
        cat ".tmp/fuzz/$target.log"
        exit 1
    fi
done
sha256sum -c .tmp/fuzz/lock.sha256
echo "Bounded address-sanitized coverage-guided Rust targets passed"
