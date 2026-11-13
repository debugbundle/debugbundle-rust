#!/bin/sh
set -eu
mode=${1:?preflight or publish required}
case "$mode" in preflight|publish) ;; *) exit 2 ;; esac
output="$PWD/.tmp/release"
mkdir -p "$output"
version=${RELEASE_VERSION:?release version required}
source_sha=${RELEASE_SOURCE_SHA:?immutable source commit required}
test "${#source_sha}" -eq 40
case "$source_sha" in *[!0-9a-f]*) exit 2 ;; esac
# This exact checkout is deliberately bind-mounted from the hosted runner into
# a root-owned container. Trust only this path for these Git calls.
actual_source=$(git -c safe.directory="$PWD" rev-parse HEAD)
test "$actual_source" = "$source_sha"
source_status=$(git -c safe.directory="$PWD" status --porcelain --untracked-files=normal)
test -z "$source_status" || { echo "Release requires a clean committed checkout" >&2; exit 1; }
test "$(cargo pkgid --locked | sed 's/.*@//')" = "$version"
sh scripts/check-candidate.sh
# Cargo 1.99 publishes from package/tmp-crate, unlike `cargo package`.
# A fresh target prevents an earlier package command from masking missing output.
package_target=$(mktemp -d "$output/$mode-package.XXXXXX")
# Only public crate files are written here. Hosted artifact upload runs as the
# checkout owner rather than the container's root user.
chmod 755 "$package_target"
if [ "$mode" = preflight ]; then
    # Cargo publish creates a package again. Pin the compiler/lock/source and
    # compare that dry-run package with the artifact every OS qualified.
    CARGO_TARGET_DIR="$package_target" cargo publish --registry crates-io --locked --no-verify --dry-run > "$output/dry-run.log" 2>&1 || { cat "$output/dry-run.log"; exit 1; }
else
    test -n "${CARGO_REGISTRY_TOKEN:-}" || { echo "Selected publishing credential is missing" >&2; exit 1; }
    test -f "$output/preflight.sha256"
    test "$(cat "$output/preflight.sha256")" = "$DEBUGBUNDLE_CANDIDATE_SHA256"
    # This is the only external mutation in this script. Never retry it
    # automatically: an index timeout can still mean the upload succeeded.
    CARGO_TARGET_DIR="$package_target" cargo publish --registry crates-io --locked --no-verify > "$output/publish.log" 2>&1 || { cat "$output/publish.log"; exit 1; }
fi
published_artifact="$package_target/package/tmp-crate/debugbundle-$version.crate"
test -f "$published_artifact" || { echo "Cargo did not produce the publish archive: $published_artifact" >&2; exit 1; }
DEBUGBUNDLE_CANDIDATE_ARTIFACT="$published_artifact" sh scripts/check-candidate.sh
if [ "$mode" = preflight ]; then
    printf '%s\n' "$DEBUGBUNDLE_CANDIDATE_SHA256" > "$output/preflight.sha256"
fi
echo "Release $mode package matches the qualified candidate"
