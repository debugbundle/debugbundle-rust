#!/bin/sh
set -eu
mkdir -p .tmp/features
cargo tree --no-default-features --locked --edges normal --prefix none > .tmp/features/core-tree.txt
if grep -E '^(axum|actix-web|tower|tracing-subscriber|reqwest) v' .tmp/features/core-tree.txt; then
    echo "Core-only build pulled an optional framework, HTTP or subscriber dependency" >&2
    exit 1
fi
echo "Core-only normal dependency graph excludes optional HTTP/framework/subscriber stacks"
