# Release qualification and ownership

The SDK is an unpublished candidate. Preparing these files does not publish,
commit or push it. Release and consumer adoption require separate instructions.

`SDK qualification` builds one immutable `rust-candidate` artifact, records its
SHA-256 and passes those bytes to Linux x64/arm64, native macOS/Windows and the
Browser relay consumers. Every selected-artifact lane rejects a checksum mismatch.
Source/MSRV/feature/coverage/fuzz/resource checks remain distinct receipts. All
required jobs must pass on the release commit; local Linux checks cannot replace
native platform qualification.

Before the first release, the owner must confirm the public `debugbundle` crate
name and chosen crates.io ownership. `make crate-namespace` only reads public
state: a 404 neither reserves the name nor assigns ownership. First publication
requires a narrowly scoped bootstrap API token: trusted publishing requires an
existing crate and owner configuration. After the first publication, configure
trusted publishing for `debugbundle/debugbundle-rust`, workflow `release.yml`,
environment `crates-io`, and remove/revoke the bootstrap token. These are manual
owner actions, not performed by qualification.

Create the GitHub `crates-io` environment with required owner review before
enabling publication. Keep `CRATES_IO_BOOTSTRAP_TOKEN` only in that environment
when bootstrap is explicitly selected; subsequent releases select `trusted` and
use the short-lived token from the official action. No token is embedded in
artifacts or command arguments. A missing selected credential fails rather than
falling back to another authentication mode.

For an explicitly authorised release, use `Manual crate release` on the exact
qualified `main` commit, entering the reviewed version and candidate SHA-256 from
that commit's qualification run. The workflow reruns the complete qualification
matrix, compares the entered identity, then waits for environment approval. It
does not publish on push, pull request, a tag, or qualification alone. No tag or
GitHub release is created automatically.

The publish job uses the same digest-pinned compiler/container and frozen lockfile
as candidate creation. It requires a clean committed checkout at the immutable
workflow SHA. Cargo's dry-run publish repackages the source; that result must match
the qualified candidate before obtaining a token. The live standard Cargo publish
uses the same locked source and is checked again afterward. The registry's index
checksum and downloaded `.crate` must also match. Cargo does not expose a command
to upload a prebuilt archive, so these reproducibility and registry checks are
essential; a successful command alone is not an exact-artifact receipt.

`RELEASE_VERSION=... RELEASE_SHA256=... make smoke-published` reads the public
index, downloads the exact version, verifies its checksum/non-yanked state and
records public ownership. It creates fresh consumers with an exact registry
version and no SDK path override, records the new public dependency resolution,
then freezes it before running vanilla, Axum and non-Send Actix applications.
All runtime traffic stays on isolated container loopback. These consumers can
resolve newer compatible dependencies than the local artifact lane; their actual
lock and metadata are retained rather than assumed equivalent. Use a fresh
checkout/qualification directory for each registry run. This lane is intentionally
unrun while the crate is unpublished.

If publish times out or any subsequent checksum/consumer gate fails, preserve the
receipts and inspect public registry state before taking any further publishing
action. Cargo can time out after a successful upload. Do not automatically retry,
yank, change ownership, replace a release or update consumers. A checksum mismatch
after upload is a release incident requiring an explicit owner decision.

References: [Cargo publish](https://doc.rust-lang.org/cargo/commands/cargo-publish.html),
[registry index](https://doc.rust-lang.org/cargo/reference/registry-index.html),
[trusted publishing prerequisites](https://crates.io/docs/trusted-publishing),
[official authentication action](https://github.com/rust-lang/crates-io-auth-action/blob/v1.0.6/README.md).
