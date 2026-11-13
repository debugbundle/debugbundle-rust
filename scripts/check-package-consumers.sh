#!/bin/sh
set -eu

mode=${1:-verify}
case "$mode" in lock|verify|export|browser|lower-lock|lower) ;; *) exit 2 ;; esac
source_root=$PWD
version=$(cargo pkgid --locked | sed 's/.*@//')
case "$version" in ''|*[!0-9A-Za-z.-]*) echo "Invalid package identity" >&2; exit 1 ;; esac
artifact=${DEBUGBUNDLE_CANDIDATE_ARTIFACT:-"$CARGO_TARGET_DIR/package/debugbundle-$version.crate"}
if [ -n "${DEBUGBUNDLE_CANDIDATE_ARTIFACT:-}" ]; then
    sh scripts/check-candidate.sh
fi
test -f "$artifact"
if [ "$mode" = export ]; then
    mkdir -p "$source_root/.tmp/packages"
    cp "$artifact" "$source_root/.tmp/packages/debugbundle-$version.crate"
    sha256sum "$artifact" "$source_root/Cargo.lock" > "$source_root/.tmp/packages/checksums.txt"
    rustc -vV > "$source_root/.tmp/packages/toolchain.txt"
    echo "Prepared local candidate: .tmp/packages/debugbundle-$version.crate"
    exit 0
fi
qualification_root=$(mktemp -d /tmp/debugbundle-crate.XXXXXXXX)
output="$source_root/.tmp/package-qualification"
case "$mode" in lower|lower-lock) output="$output/lower" ;; esac
mkdir -p "$output" "$qualification_root/artifact" "$qualification_root/consumers"

tar -tzf "$artifact" > "$output/contents.txt"
while IFS= read -r entry; do
    case "$entry" in "debugbundle-$version/"*) ;; *) echo "Unexpected package path" >&2; exit 1 ;; esac
    case "$entry" in *../*|/*) echo "Unsafe package path" >&2; exit 1 ;; esac
done < "$output/contents.txt"
tar -xzf "$artifact" -C "$qualification_root/artifact"
extracted="$qualification_root/artifact/debugbundle-$version"
test -f "$extracted/Cargo.toml"
test -f "$extracted/LICENSE"
test -f "$extracted/README.md"
test -f "$extracted/CHANGELOG.md"
test -f "$extracted/docs/runtime-and-api.md"

cp -R "$source_root/smoke/package-consumers/." "$qualification_root/consumers/"
sed "s|__SDK_PATH__|$extracted|g" "$source_root/smoke/package-consumers/Cargo.toml.in" > "$qualification_root/consumers/Cargo.toml"
cp "$source_root/tests/fixtures/relay-compliance.json" "$qualification_root/consumers/support/relay-compliance.json"
cd "$qualification_root/consumers"
export CARGO_BUILD_JOBS=1
export RUST_TEST_THREADS=1
export DEBUGBUNDLE_ARTIFACT_EVENTS_OUTPUT="$output/rust-events.json"
if [ "$mode" = lower-lock ]; then
    cargo update --offline -p tokio --precise 1.53.3
    cargo update --offline -p tower --precise 0.5.2
    cp Cargo.lock "$source_root/smoke/package-consumers/Cargo.lower.lock"
    echo "Prepared lower consumer resolution; runtime qualification is still required."
    exit 0
fi
if [ "$mode" = lower ]; then
    cp "$source_root/smoke/package-consumers/Cargo.lower.lock" Cargo.lock
fi
if [ "$mode" = lock ]; then
    cargo generate-lockfile --offline
    cp Cargo.lock "$source_root/smoke/package-consumers/Cargo.lock"
    echo "Prepared consumer resolution; runtime qualification is still required."
    exit 0
fi
test -f Cargo.lock
# Metadata enumerates the frozen cross-platform dependency graph. Populate that
# exact graph once, then keep all compilation/runtime commands offline.
cargo fetch --locked
cargo metadata --locked --offline --format-version 1 > "$output/consumer-metadata.json"
if grep -F 'file:///work' "$output/consumer-metadata.json" >/dev/null; then
    echo "Consumer resolution points back to the source checkout" >&2
    exit 1
fi
rustc -vV > "$output/toolchain.txt"
cargo --version >> "$output/toolchain.txt"
sha256sum "$artifact" "$source_root/Cargo.lock" Cargo.lock > "$output/checksums.txt"
if [ "$mode" = browser ]; then
    timeout 600 cargo build --locked --offline -p artifact-browser-host
    mkdir -p "$source_root/.tmp/browser-qualification"
    cp "$CARGO_TARGET_DIR/debug/artifact-browser-host" "$source_root/.tmp/browser-qualification/host"
    cp "$output/checksums.txt" "$output/consumer-metadata.json" "$output/toolchain.txt" "$source_root/.tmp/browser-qualification/"
    echo "Browser host built against the extracted candidate, not checkout source"
    exit 0
fi
# Run the complete functional suite from packaged files as well as independent
# applications. Missing fixtures or checkout-only test dependencies must fail.
if [ "$mode" = lower ]; then
    timeout 600 cargo build --workspace --all-targets --locked --offline
elif timeout 1200 cargo test --manifest-path "$extracted/Cargo.toml" --all-features --locked --offline > "$output/packaged-tests.log" 2>&1; then
    tail -n 6 "$output/packaged-tests.log"
else
    cat "$output/packaged-tests.log"
    exit 1
fi
for package in artifact-vanilla artifact-axum artifact-actix; do
    if timeout 600 cargo run --locked --offline -p "$package" > "$output/$package.log" 2>&1; then
        tail -n 3 "$output/$package.log"
    else
        cat "$output/$package.log"
        exit 1
    fi
done
if [ "$mode" = lower ]; then
    echo "Extracted-crate Tokio 1.53.3 / Tower 0.5.2 consumer runtime acceptance passed"
    exit 0
fi
mkdir -p "$qualification_root/panic"
cp -R "$source_root/smoke/panic-profiles/." "$qualification_root/panic/"
sed "s|path = \"../..\"|path = \"$extracted\"|" "$source_root/smoke/panic-profiles/Cargo.toml" > "$qualification_root/panic/Cargo.toml"
cargo metadata --manifest-path "$qualification_root/panic/Cargo.toml" --locked --offline --format-version 1 > "$output/panic-metadata.json"
if grep -F 'file:///work' "$output/panic-metadata.json" >/dev/null; then
    echo "Panic consumer resolves back to the source checkout" >&2
    exit 1
fi
if ! timeout 600 cargo run --manifest-path "$qualification_root/panic/Cargo.toml" --locked --offline -- unwind > "$output/panic-unwind.log" 2>&1; then
    cat "$output/panic-unwind.log"
    exit 1
fi
if ! timeout 600 cargo build --manifest-path "$qualification_root/panic/Cargo.toml" --locked --offline --profile abort > "$output/panic-abort-build.log" 2>&1; then
    cat "$output/panic-abort-build.log"
    exit 1
fi
set +e
"$CARGO_TARGET_DIR/abort/debugbundle-panic-profile-smoke" abort > "$output/panic-abort.stdout" 2> "$output/panic-abort.stderr"
panic_status=$?
set -e
test "$panic_status" -eq 134
grep -Fxq previous-hook "$output/panic-abort.stderr"
echo "Exact-artifact unwind and abort behaviour preserved"
echo "Exact local crate consumers passed; evidence: .tmp/package-qualification"
