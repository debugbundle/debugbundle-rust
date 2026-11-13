#!/bin/sh
set -eu
ulimit -c 0
cargo run --manifest-path smoke/panic-profiles/Cargo.toml --locked -- unwind
cargo build --manifest-path smoke/panic-profiles/Cargo.toml --locked --profile abort
mkdir -p .tmp/panic-profiles
set +e
"${CARGO_TARGET_DIR}/abort/debugbundle-panic-profile-smoke" abort >.tmp/panic-profiles/abort.stdout 2>.tmp/panic-profiles/abort.stderr
panic_status=$?
set -e
test "$panic_status" -eq 134
grep -Fxq previous-hook .tmp/panic-profiles/abort.stderr
printf '%s\n' 'abort-termination-preserved'
