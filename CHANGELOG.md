# Changelog

## Unreleased — 1.0.0-alpha.1

Initial Rust SDK candidate: bounded primitive capture, mandatory privacy, typed
exception evidence, strict indexed delivery, private local arrays, revocable
operation/task context, probes, authenticated configuration, signed triggers,
opt-in recovery, composable tracing/log adapters and explicit panic observation.

Axum/Tower and Actix Web integrations preserve native service/future/body
behaviour. Browser relay factories provide protected canonical ingress, origin
validation, bounded uploads, local/forwarded delivery and an authenticated durable
spool with per-index ACK progress. A client-owned shared limiter interface supports
replica-aware application stores.
Known relay acknowledgements retain their bounded record/file lease through
callback pressure and persistence failures. Exact progress/completion retries are
idempotent after lost callback results; shutdown remains finite.

Private storage has descriptor-relative Unix operations and a narrow Windows
Win32 boundary with private DACLs, pinned ancestors and reparse refusal. Windows
source cross-compiles under Rust 1.89. Native acceptance is recorded per exact
candidate; cross-compilation alone does not establish runtime/coverage safety.
Composed snapshots preserve causes/aggregate children, unsafe limitations and
honest UTF-8/graph truncation. Full queues skip lazy producers even while native
HTTP is stalled and the callback lane is free.

Candidate archives retain only the approved source, fixtures and documentation;
local dependencies and hidden metadata are excluded and rejected before extraction.

Local Linux qualification includes exact-crate consumers, real Core ingestion and
local CLI processing, published Browser-to-both-Rust-relay checks, per-file line
coverage, bounded sanitizer fuzzing and measured resource/cost regression budgets.
The baseline candidate passed Linux x64/arm64, native macOS/Windows and extracted-
artifact qualification. The deep audit adds full nested snapshot graph remapping,
bounded public privacy-key matching, explicit Rustls and disabled automatic
response decompression even when an application enables reqwest compression.
Real certificate-chain, hostname, expiry, redirect and stalled-handshake tests
plus a feature-unified gzip regression run with the packaged test suite.
Rejected or capacity-exhausted configuration now disposes application hooks,
transports and limiter stores through separate unwind boundaries, preventing an
initialization failure from escaping or double-panicking across SDK fields.
Synchronous bootstrap limits remain explicit; admitted-client cleanup is deferred.
Disabled native logging without an application sink returns before the record-
building frame and recursion guard. Application tee filtering, callbacks and flush
remain active when SDK capture is disabled or detached. Existing performance
budgets and benchmark samples remain unchanged.
Rejected recursive calls preserve the outer guard for native log/tracing
adapters and panic observation, including repeated application-bridge attempts.
The audit candidate requires fresh qualification; publication remains on hold.
No package publication or consumer adoption is implied.
