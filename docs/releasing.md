# Release qualification and ownership

Rust follows the SDK release pattern: a `v*` version tag starts `Crate release`.
The version must already match Cargo.toml, and the exact source commit must have
passed the complete eight-job `SDK qualification` workflow on main. A tag is an
explicit release action; ordinary main pushes qualify source without publishing.
`workflow_dispatch` is retained for recovery, with an optional `release_ref`
(default `main`). No manual version, checksum or authentication selection is needed.

The release planner resolves the source on main's history, checks the latest
qualification for that exact commit, and requires all eight jobs to have passed.
It downloads that run's immutable `rust-candidate`, verifies its package/VCS
identity and checksum receipt, then derives the release inputs. An active source
qualification is awaited, not duplicated. Failed, incomplete, fork/PR, different-
source and expired/unavailable artifact evidence cannot qualify a release.

The qualified SDK checkout is separate from the release-tooling checkout. A
release-tooling repair can therefore publish an already-qualified SDK commit
without changing or rebuilding its archive. Current tooling repackages that clean
source using its own pinned compiler and lockfile, and compares the resulting
bytes before any publishing credential is used. Git's ownership exception is
command-scoped to that deliberate container checkout, never a global wildcard.
Each publish/dry-run gets a fresh target directory and checks Cargo 1.99's actual
`package/tmp-crate` output; a cached `cargo package` archive cannot qualify it.
Every new SDK source/artifact still requires its own complete native qualification.

The `crates-io` GitHub environment permits main recovery and version tags. In the
owner-selected automated release configuration it has no per-release reviewer
prompt, matching the other SDK distribution workflows. Only an authorised version
tag or explicit recovery dispatch starts publication; qualification alone does not.

First publication uses the environment's `CRATES_IO_BOOTSTRAP_TOKEN` only while
the public crate namespace is absent. Once the crate exists, the workflow selects
Trusted Publishing for `debugbundle/debugbundle-rust`, workflow `release.yml`,
environment `crates-io`. Configure that identity on crates.io after bootstrap,
then revoke/remove the bootstrap token. A trusted-authentication failure never
falls back to the bootstrap credential. Initial account/registry ownership is a
one-time prerequisite, not a per-release form.

After registering the identity, verify it without publishing another version:
`gh workflow run release.yml --repo debugbundle/debugbundle-rust -f verify_publisher_only=true`.
This uses the same workflow/environment identity, obtains the temporary credential
and revokes it in the official action's cleanup. No SDK qualification or crate
upload runs in this mode.

An existing, non-yanked version with the same public checksum selects verification
only and never uploads again. A mismatched/yanked version fails closed. There is
no automatic upload retry: an upload can succeed before Cargo reports a timeout.
Inspect public state and preserve receipts before recovery, yanking, changing
ownership or taking any further publication action.

`make smoke-published` verifies the public index checksum, downloaded crate and
owners, then builds real exact-version registry consumers with no SDK path
override. Vanilla, Axum and genuinely non-Send Actix applications run on isolated
loopback. Their actual new registry resolution is retained and frozen before
execution; it is not assumed equal to the development lockfile. Only after this
acceptance does the workflow create the normal versioned GitHub release, with a
prerelease flag for alpha/beta versions. Existing version tags are never retargeted.

Release diagnostics are created before preflight and retained on early failure,
including container stderr, exit status and timeout output. Missing diagnostic
files cannot replace the primary failure with an artifact-upload error. The SDK
archive and source identity remain separate from the release-tooling identity.

## First publication receipt

`1.0.0-alpha.1` is published from `eb9e09afc02751ebd34464bfa4b8808a565635b4`,
with SHA-256 `1aed6405f1b6b80ef9430d8300e9efe330a9e06be8c51587da0a41fe07abd8bc`.
Run `43530643791` passed preflight, bootstrap upload, public checksum/download
and real registry-installed standalone/Axum/Actix applications. Its final GitHub
release creation received HTTP 403 under the workflow integration; the owner CLI
completed that same-source prerelease and asset without uploading to crates.io again.
The already-published verification recovery is `43531207396`. Future tag releases
already have their source tag before finalisation; never grant a broader workflow
credential just to recover a historical source tag.

The GitHub environment and tag automation are configured. The one-time crates.io
Trusted Publisher registration and its first OIDC exchange are not yet verified.
Do not claim future unattended publication is operational until that identity is
registered and checked. The bootstrap token is never used for existing crates.

References: [Cargo publish](https://doc.rust-lang.org/cargo/commands/cargo-publish.html),
[registry index](https://doc.rust-lang.org/cargo/reference/registry-index.html),
[trusted publishing](https://crates.io/docs/trusted-publishing),
[official authentication action](https://github.com/rust-lang/crates-io-auth-action/blob/v1.0.6/README.md).
