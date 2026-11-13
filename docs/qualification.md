# Candidate qualification

This is an unpublished SDK candidate. Every source change requires a fresh [eight-job CI run](https://github.com/debugbundle/debugbundle-rust/actions/workflows/ci.yml) and artifact seal, including Linux x64/arm64, native macOS/Windows and consumers of one immutable crate. Match the commit and checksum in that run before relying on its acceptance. Running local Linux tests alone never establishes other platforms, registry publication or consumer adoption.

Historical baseline `244c104` passed [all eight jobs](https://github.com/debugbundle/debugbundle-rust/actions/runs/43406799897). It does not qualify later audit changes.

`make verify test-core test-loggers test-panic test-msrv test-current coverage audit` owns the
source gates. Coverage measures executable lines in every production module for
the actual target, requires every compiled source record, and fails below 80
percent per file. Linux excludes the two Windows-only modules; native Windows
coverage includes those and excludes the Unix-only implementation. Neither target
can establish coverage of the other platform's OS calls.
Test helpers are excluded. This stable LLVM lane does not establish branch or
function coverage. Dependency policy has no advisory suppressions.

`test-loggers` executes the native fixtures with `log` and `tracing` independently,
without default HTTP features. Fixtures observe best-effort admission with a finite
deadline, retain exact application-sink counts per attempt and separate canonical
operational pressure summaries from the requested records.

`make package` exports and validates the actual archive before accepting it as a
candidate. Anchored includes retain Rust source, canonical fixtures and documented
examples while excluding unrelated development files. The portable extraction
guard rejects unexpected inventory, hidden files, dependency trees, unsafe paths,
links, devices and aliases before copying. Its adverse fixtures run without Rust.

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

Local Docker Desktop runs select `baselines/linux-aarch64.json`; the GitHub
`ubuntu-24.04-arm` job explicitly selects `baselines/github-linux-aarch64.json`
through `PERFORMANCE_BASELINE` (both under `smoke/performance/`). The latter
records three Neoverse-N2 repeats from measurement-only run
[`43393447198`](https://github.com/debugbundle/debugbundle-rust/actions/runs/43393447198),
with identical SDK/benchmark source at `9e4bb48`, report hashes and host/compiler
provenance. Both use the same budget formula and unchanged hard resource caps.
The disabled Axum control's measured p99 varies from 2,608 to 10,161 ns across
those repeats; it includes framework and instrumentation overhead. Its 41,000 ns
CI guard is a regression budget, not SDK capture cost. Most active-case latency
budgets are tighter on this host. CI never regenerates a baseline automatically;
a source, workload or runner change requires inspected measurements and review.

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
fuzzing or a formal concurrency model. Native-platform file/relay and exact-artifact checks are CI gates for every candidate. Deep audit fixes must pass them before a later, separately approved release.

## TLS handshake coverage

`transport::tests` uses a real Rustls loopback server and synthetic root,
intermediate and leaf certificates. The production transport builder and
request/response path exercise authenticated event/config traffic, wrong hostname,
untrusted root, missing intermediate, expired leaf, redirect refusal and a stalled
handshake. Positive fixtures add a root only to their private client using
reqwest's native-root merge; they never modify the machine trust store or disable
certificate validation. No custom-root configuration API or alternate TLS backend
is advertised. These tests run in source and extracted-crate suites on each
native CI target. `make tls-fixtures` regenerates only the synthetic certificate
JSON offline; the server private key is test material and is never trusted outside
this fixture.

The packaged HTTP regression suite enables reqwest gzip as a development-only
consumer feature. It proves an application client can decode a real compressed
response while the SDK rejects it for both ingestion and configuration. This
protects the response-size/encoding boundary from Cargo feature unification.
`make audit-test-lock` resolves added test-only dependencies; routine gates use
`--locked`. Test dependencies do not enter a normal core-only consumer graph.
