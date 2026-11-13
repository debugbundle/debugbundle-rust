#!/bin/sh
set -eu
version=$(cargo pkgid --locked | sed 's/.*@//')
case "$version" in ''|*[!0-9A-Za-z.-]*) echo "Invalid package identity" >&2; exit 1 ;; esac
artifact=${DEBUGBUNDLE_CANDIDATE_ARTIFACT:?explicit candidate path required}
expected=${DEBUGBUNDLE_CANDIDATE_SHA256:?explicit candidate checksum required}
test "${#expected}" -eq 64
case "$expected" in *[!0-9a-f]*) echo "Invalid candidate checksum" >&2; exit 1 ;; esac
test "$(basename "$artifact")" = "debugbundle-$version.crate"
test -f "$artifact"
test "$(wc -c < "$artifact")" -le 8388608
actual=$(sha256sum "$artifact" | cut -d ' ' -f 1)
test "$actual" = "$expected" || { echo "Candidate checksum mismatch" >&2; exit 1; }
echo "Verified selected candidate $version: $actual"
