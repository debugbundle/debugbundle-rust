#!/bin/sh
set -eu
mode=${1:?prepare or run required}
case "$mode" in prepare|run) ;; *) exit 2 ;; esac
version=${RELEASE_VERSION:?release version required}
case "$version" in ''|*[!0-9A-Za-z.-]*) exit 2 ;; esac
source_root=$PWD
output="$source_root/.tmp/published-registry"
consumer="$output/consumers"
export CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1
export DEBUGBUNDLE_ARTIFACT_EVENTS_OUTPUT="$output/rust-events.json"
if [ "$mode" = prepare ]; then
    test ! -e "$consumer" || { echo "Use a fresh qualification directory for registry consumers" >&2; exit 1; }
    mkdir -p "$consumer"
    cp -R "$source_root/smoke/package-consumers/." "$consumer/"
    sed "s|path = \"__SDK_PATH__\"|version = \"=$version\"|" "$consumer/Cargo.toml.in" > "$consumer/Cargo.toml"
    cp "$source_root/tests/fixtures/relay-compliance.json" "$consumer/support/relay-compliance.json"
    # This is a fresh public resolution, intentionally independent of path-based
    # qualification. Freeze it before executing any installed application.
    rm "$consumer/Cargo.lock"
    cd "$consumer"
    cargo generate-lockfile
    cargo fetch --locked
    cargo metadata --locked --offline --format-version 1 > "$output/consumer-metadata.json"
    cp Cargo.lock "$output/consumer.lock"
    rustc -vV > "$output/toolchain.txt"
    exit 0
fi
cd "$consumer"
cmp Cargo.lock "$output/consumer.lock"
for package in artifact-vanilla artifact-axum artifact-actix; do
    if timeout 600 cargo run --locked --offline -p "$package" > "$output/$package.log" 2>&1; then
        tail -n 3 "$output/$package.log"
    else cat "$output/$package.log"; exit 1; fi
done
cmp Cargo.lock "$output/consumer.lock"
echo "Genuine crates.io vanilla, Axum and non-Send Actix consumers passed"
