#!/bin/sh
set -eu
mode=${1:?preflight or publish required}
case "$mode" in preflight|publish) ;; *) exit 2 ;; esac
version=${RELEASE_VERSION:?release version required}
source_sha=${RELEASE_SOURCE_SHA:?immutable source commit required}
test "${#source_sha}" -eq 40
case "$source_sha" in *[!0-9a-f]*) exit 2 ;; esac
test "$(git rev-parse HEAD)" = "$source_sha"
test -z "$(git status --porcelain --untracked-files=normal)" || { echo "Release requires a clean committed checkout" >&2; exit 1; }
test "$(cargo pkgid --locked | sed 's/.*@//')" = "$version"
sh scripts/check-candidate.sh
output="$PWD/.tmp/release"
mkdir -p "$output"
if [ "$mode" = preflight ]; then
    # Cargo publish creates a package again. Pin the compiler/lock/source and
    # compare that dry-run package with the artifact every OS qualified.
    cargo publish --registry crates-io --locked --no-verify --dry-run > "$output/dry-run.log" 2>&1 || { cat "$output/dry-run.log"; exit 1; }
else
    test -n "${CARGO_REGISTRIES_CRATES_IO_TOKEN:-}" || { echo "Selected publishing credential is missing" >&2; exit 1; }
    test -f "$output/preflight.sha256"
    test "$(cat "$output/preflight.sha256")" = "$DEBUGBUNDLE_CANDIDATE_SHA256"
    # This is the only external mutation in this script. Never retry it
    # automatically: an index timeout can still mean the upload succeeded.
    cargo publish --registry crates-io --locked --no-verify > "$output/publish.log" 2>&1 || { cat "$output/publish.log"; exit 1; }
fi
DEBUGBUNDLE_CANDIDATE_ARTIFACT="$CARGO_TARGET_DIR/package/debugbundle-$version.crate" sh scripts/check-candidate.sh
if [ "$mode" = preflight ]; then
    printf '%s\n' "$DEBUGBUNDLE_CANDIDATE_SHA256" > "$output/preflight.sha256"
fi
echo "Release $mode package matches the qualified candidate"
