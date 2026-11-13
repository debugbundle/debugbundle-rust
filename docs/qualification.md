# Candidate qualification

This is an unpublished SDK candidate. Local Linux arm64 qualification does not
establish native macOS, Windows or Linux x64 acceptance. The public repository
exists; no source commit, push, registry publication or consumer adoption is
implied by these commands.

`make verify test-core test-panic test-msrv test-current coverage audit` owns the
source gates. Coverage measures executable lines in every production module for
the actual target, requires every compiled source record, and fails below 80
percent per file. Linux excludes the two Windows-only modules; native Windows
coverage includes those and excludes the Unix-only implementation. Neither target
can establish coverage of the other platform's OS calls.
Test helpers are excluded. This stable LLVM lane does not establish branch or
function coverage. Dependency policy has no advisory suppressions.

`make smoke-package` runs the complete packaged functional suite, then independent
vanilla, Axum and genuinely non-Send Actix TCP applications against an extracted
`.crate`. It rejects metadata that resolves back to the source checkout. The
separately frozen consumer lockfile is part of the receipt; it is not assumed to
equal the SDK development resolution. Unwind and abort subprocesses also consume
the extracted artifact. Receipts and checksums are under `.tmp/package-qualification`.

`make test-browser-relays` bundles the frozen published Browser 3.2.2 npm artifact
and its exact dependencies, then runs headless Chromium against the extracted
Rust crate's Axum and Actix HTTP relays. Runtime networking is disabled except
container loopback. Each route checks protected debug and opted-in analytics
traffic, including a mixed batch, against a local mock authenticated ingestion
server. Checks cover capability bootstrap, native nested
causes, unsafe cause omission, opted-in recovery, actual backend trace propagation,
preflight and forged credentials/authority. A dispatched native `pagehide` listener
also exercises real Chromium `sendBeacon`, a declined beacon's keepalive-fetch
fallback and no application hook work in the lifecycle listener. The fixture
observes completion upstream; it does not claim real-navigation unload delivery.
Browser cookies never reach ingestion;
only the server project token authenticates it. No browser credentials or secrets
are retained upstream. Receipts include browser npm integrity and compiled bundle
hash, Rust crate/lock hashes and resolved consumer metadata.

The published Browser's legacy outer-error normalizer uses guarded field reads
and invokes two getters in the deliberately opaque-object fixture. Its native
cause-evidence getter count is zero. These are recorded separately; this lane does
not claim that all Browser legacy error normalization has Rust's sealed borrowed
snapshot semantics. Ordinary relay requests carry no DebugBundle authorization
header or project token. Ambient same-origin cookie behaviour is not used as
relay authentication and is not forwarded to ingestion.

`make evidence-performance performance-check` runs an independent release-profile
executable in the pinned Rust container with two CPUs and two GiB. Its test-only
allocation instrumentation never becomes an SDK production dependency. Twenty-two
cases compare disabled, baseline and evidence capture/logging/framework paths;
real full queues hold a hook, custom transport or native HTTP response. Setup,
seed, resource reads and flush are excluded from caller timings. Formatting and
lazy producer counters remain zero under pressure. The shared diagnostic ledger
is observed below its joint 64 KiB cap. Caller-thread Rust allocation totals and
peaks are distinct from sampled SDK ownership, process RSS and OS thread count.

The Linux arm64 baseline records three measured repeats, raw-report hashes,
toolchain/container identity and source digest. Regression guards permit four
times the worst observed p99 rounded up to one microsecond, with a 2.5-microsecond
clock/noise floor. Allocation totals allow ten percent above the largest repeat;
measured zero stays exactly zero. Missing cases, wrong architecture, invalid
numbers, changed sample counts, latency/allocation regressions and exceeded
resource contracts fail closed. These instrumented development-container budgets
are not a product latency SLA; another host/architecture needs an inspected
baseline and native CI acceptance.

Native CI installs only pinned Rust 1.100.0, llvm-tools-preview for that compiler
and cargo-llvm-cov 0.9.1 on disposable macOS/Windows runners. Local development
keeps Rust, Cargo and npm work in Docker. `make windows-typecheck` cross-checks the
Windows local/testkit/log/tracing source and native test code under Rust 1.89; it
does not execute Windows binaries or prove ACL semantics. Its Windows-only FFI
boundary uses the already-frozen Microsoft windows-sys 0.61.2 package. Existing
dependency versions/checksums were preserved when linking that target dependency.

CI creates one candidate and passes its exact SHA-256 to all Linux/native artifact
consumers and Browser relay lanes. Native archive extraction rejects traversal,
links/devices, duplicate names and portable case/Unicode aliases before writing.
Pure Python extraction/digest tests and pinned Node registry-verification tests
run without Rust or network. Both workflows pass pinned actionlint validation.
See [release qualification and ownership](releasing.md) for the separate manual
publication path and genuine crates.io consumers. Those registry consumers remain
unrun until a version is explicitly published.

The bounded deterministic property corpus covers 2,560 generated privacy,
capability and indexed-ACK cases. `make fuzz` additionally uses cargo-fuzz 0.13.2,
libfuzzer-sys 0.4.13 and nightly-2026-11-12 (Rust 1.102.0-nightly). Four targets
cover canonical wire/privacy/ACKs, graph composition/opaque errors, relay body and
header validation, and private-file names. Each runs at most 4,096 mutations or
30 seconds, with 64 KiB input and bounded RSS/deadlines in a network-isolated
container. AddressSanitizer is enabled; `cfg(fuzzing)` is deliberately disabled
so production security checks remain active. The long-lived SDK runtime makes
process-exit leak detection unsuitable, so this lane disables that oracle and
retains the separate ownership/cardinality/cleanup tests. It is not exhaustive
fuzzing or a formal concurrency model. Native-platform file/relay qualification, additional adverse
filesystem/runtime gates and the final exact-artifact/release review remain open;
see the core implementation record for the full outstanding matrix.
