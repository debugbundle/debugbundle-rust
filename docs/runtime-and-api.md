# Rust API and runtime boundaries

SDK `1.0.0` supports Linux x64/arm64, macOS and Windows. Every release requires
source and extracted-crate qualification for its exact commit and archive;
registry installation and consumer adoption remain separate checks. See
[qualification](qualification.md).

## Client and capture

`Client::new(Config)` and `init(Config)` create the same isolated client. Configure
`project_token`, `service`, `environment` and optionally a self-hosted HTTPS
`endpoint`. `Config::from_env()` reads only `DEBUGBUNDLE_PROJECT_TOKEN`,
`DEBUGBUNDLE_ENDPOINT`, `DEBUGBUNDLE_SERVICE` and `DEBUGBUNDLE_ENVIRONMENT`.
`Config::local(service, directory)` selects local-only file output and requires
the `local` feature. A local-only client rejects a custom network transport.

Construct clients once during application bootstrap. `Client::new` / `init` are
synchronous setup operations: runtime/TLS initialization and disposal of rejected
configuration values happen before they return. Rejected configuration releases
custom hooks, transports and limiter stores behind separate unwind boundaries,
so a destructor panic does not escape or double-panic across SDK-owned fields.
Application destructors that block or abort cannot be time-bounded or sandboxed;
keep these setup calls out of request/capture paths. Rejection does not create
replacement workers or retain an unbounded overflow queue. Successful-client
cleanup continues on the existing bounded cleanup worker.

`capture_exception` / `capture_error` accept a borrowed `ExceptionSnapshot` or
`ExceptionInput::borrowed(&error)`. Explicit snapshots provide protected names,
messages, supplied stacks, causes, aggregate children and `with_frames` structured
frames. `TraceOrigin` identifies capture, runtime-reported, unknown or unavailable
origin. No automatic stack capture, symbolisation or invented throw site occurs.
Cause and aggregate builders compose in either order. An existing cause is kept;
aggregate calls append complete protected child graphs, including nested causes and aggregate relationships, within the same five-edge, eight-node, four-edge-depth and 32 KiB limits. A child graph that cannot fit is omitted with truncation and `budget_exceeded`; previously accepted nodes remain intact.
UTF-8 field clipping also reports truncation. Child limitations propagate through
composition; an `unsafe` reason remains visible when the graph also reaches a
budget, including fallback to its retained primary node.
Borrowed errors never call `Display`, `Debug` or `Error::source`; their details
remain unavailable. This also applies to arbitrary `anyhow` and `thiserror` chains.

`probe_lazy` schedules at most one protected producer per client on its bounded
callback worker. Disabled/ineligible probes and exhausted event count/byte
ownership skip the producer; capacity is checked again immediately before
invocation. An eager `probe` can still retain its already supplied protected value
in the independent bounded probe ring.

`capture_log(Level, message, context)` / `capture_message(message, Level, context)`
accept already available text. `capture_request(&RequestSnapshot, context)` takes
an explicit safe snapshot: request bodies are absent unless supplied and protected
bodies above 8 KiB are omitted. `capture_deploy(&DeploySnapshot)` records explicit
deployment metadata. Capture methods return no delivery guarantee.

`Context::new(&Value)` copies a bounded object; `with` adds protected fields.
`client.set_context(&context)` replaces the client's protected persistent metadata;
active operation and per-capture context overlay it. It does not install a global
request scope. Use request/operation context for concurrent per-request values.
Mandatory credential rules always apply and `redact_fields` only extends them.
Trace/session helpers accept opaque UUIDv4 values. `with_request_id` accepts a
nonempty printable ASCII identifier up to 128 bytes; values changed by mandatory
privacy are omitted. `with_evidence_reference` adds
an explicit typed failure/scope; `with_failure_ref` uses its active operation.
`new_evidence_id()` creates a fallible fresh UUIDv4; never substitute a customer ID,
URL or resource identifier. Invalid typed custom context never invalidates an
otherwise admissible primary error.

Defaults are 25 events per batch, a five-second timer, 1,000 retained events,
8 MiB retained event bytes and ten-minute age. Count/bytes include in-flight events
and finalized tails. Capture uses non-waiting admission and may lose telemetry
under contention or overload. Operational loss summaries are bounded. Sampling
zero remains zero. Optional evidence yields to primary errors.

`flush` / `flush_async` and `shutdown` / `shutdown_async` take a finite timeout,
capped at thirty seconds. Their `FlushOutcome.drained` means owned work drained;
`accepted_events` separately counts canonical acceptance. A drop/rejection can
drain without delivery. `status`, `log_enabled` and `last_event_at` are local reads;
the last-event timestamp advances only for accepted records. Last-handle `Drop`
revokes the client without network, file I/O, joining or application destructors on
its caller. Explicit shutdown attempts the bounded flush first.

The process has one SDK I/O runtime, at most eight clients/callback workers and one
cleanup worker. Hung application hooks/producers/transports occupy their existing
slot; no replacement worker is created. Pending cleanup also retains its client
permit. Synchronous flush does not drive the application's executor. Calls made
inside SDK callbacks cannot recursively capture or wait for their own flush.
Callback-returned JSON is checked before schema traversal and discarded iteratively
when rejected, so excessively deep owned values cannot trigger recursive JSON
destruction. Unknown oversized hook replacements are withheld; bounded invalid
replacements retain the safe-original fallback.
Optional exception evidence, execution/resource metadata and its typed reference
share a 32 KiB serialized budget. If they exceed it, optional data yields before
the primary is discarded. Owner-hook message changes remain in force; a budget
limit never restores the previous message. Reduction records bounded evidence
limit markers and removes the typed reference with its omitted evidence.
Worker isolation bounds scheduling; it does not sandbox application code or make
a callback's process abort recoverable. A pending lazy producer remains charged
until its owned reference has been disposed on the cleanup worker, including when
the callback worker becomes unavailable before execution.

## Operations and recovery

`start_operation(OperationOptions)` returns an owner with a fresh ID, monotonic
start, kind, optional technical name and optional configured limit. `scope(future)`
enters context only during each poll. `run(closure)` is the synchronous business
wrapper. `handle().wrap(future)` explicitly propagates context to spawned work.
`context()` supplies an owned context. Completion, `stop`, parent completion and
client shutdown revoke new automatic execution/diagnostic admission. Stale clones
cannot create a fresh implicit owner. Operation registries have 1,024-owner,
256 KiB metadata and 32-level nesting limits.

`capture_recovery(&RecoveryObservation, context)` is disabled unless
`capture_recovery_observations` is true, resolved policy permits diagnostics and
the entire reader path proves `recovery_v1`. It normally uses the current owned
operation; an explicit `EvidenceScope` must validate and cannot contradict a
supplied operation. An explicit page scope also requires matching session
correlation. All failure/attempt/scope identifiers are UUIDv4.

States are `AttemptStarted`, `AttemptCompleted`, `AttemptFailed`,
`RecoveryObserved` and `FailurePersisted`. Only `RecoveryObserved` asserts semantic
recovery. HTTP 200 or completion alone does not. Recovery never performs a retry,
resolves an incident or changes a fingerprint. Optional operation/status/duration
are bounded technical values. An explicit occurrence time is checked against the
five-minute association and sixty-second future-admission limits, including after
owner hooks.

The ledger shares 64 KiB between pending diagnostic events and association
metadata. Limits are fifty failure windows, five attempts and twenty observations
per failure. ACK/drop frees pending bytes without refunding allowances. Valid
hook identity/type changes transfer ownership; duplicate IDs, invalid scopes or
over-budget replacements fail closed.

## Probes and remote configuration

`probe(label, &Value, ProbeOptions, context)` keeps protected recent data in bounded
per-label rings. Scalars, arrays and null are object-wrapped. Defaults are fifty
labels and ten entries each; the protocol's 500-item inline array retains normal
application-value limits. `probe_flush_on_error` defaults true. Rings clear only
when their inline block is actually retained with an admitted exception. Oversized
optional diagnostics may be omitted while preserving the primary error.

`probe_lazy(label, &Arc<Producer>, options, context)` schedules at most one pending
producer per client on its existing callback worker. `Producer: Fn() -> Value +
Send + Sync + 'static`; borrowing request-local values is unsupported. Results
are protected before retention. Disabled, full, stale and inactive-heavy calls
invoke no producer. `ProbeOptions { heavy: true }` requires a matching active
directive. Explicit flush accounts for a pending producer, but error capture never
waits for it.

SDK-owned authenticated HTTP performs bounded initial configuration discovery.
Unknown/failed configuration starts minimal; a successful legacy response without
`capture_policy` uses balanced. Free/disabled configuration stops automatic polling.
Paid polling defaults to sixty seconds and tightens to fifteen seconds for active
directives, with ETag/304 and local expiry. Unknown discovery gets at most three
attempts. `remote_config_ready()` reports a valid discovered snapshot;
`refresh_remote_config_async(timeout)` requests an administrative bounded refresh.
No capture call fetches configuration. Custom/local readers retain the established
balanced legacy policy and do not perform implicit HTTP discovery.

`bind_probe_trigger(operation_handle, header, query)` locally validates the borrowed
token with the signing key from authenticated config. Pass the exact
`X-DebugBundle-Probe-Trigger` header and `_debug_probe` query value; a present header
has precedence, including an invalid one. No project-token signing-key fallback
exists. Signature, UUID, label/service/environment and TTL are checked. Passive
TTL is at most one hour; trigger TTL is at most twenty-four hours. The binding
belongs to one active request owner (and its owned descendants), never another
request; a nested request has its own boundary. Key rotation revokes old bindings.
Raw tokens are not retained, and captured query data is always protected.

## Native logging

With `tracing`, compose `client.tracing_layer()` into a standard
`tracing_subscriber::Registry`/`Layered` subscriber. Filtering is per layer and does
not suppress the application's other layers. The collector uses bounded primitive
field visitors and its own per-client span cache, without consulting arbitrary
subscriber span getters. Eligible span fields update in place and clear on close;
contended cleanup conservatively invalidates optional cached span context.

Use typed messages such as `tracing::error!(message = "checkout failed", code = 42)`.
Custom Debug/Display wrappers and dynamic formatting are never evaluated by the
SDK. That includes message forms routed through `record_debug`; they use an honest
unavailable marker. Typed native Error fields produce one `backend_exception`
with unavailable native details, rather than an extra log event. Explicit safe
snapshots preserve richer details.

With `log`, `client.logger()` implements `log::Log`.
`.tee(existing_sink: Arc<dyn log::Log>)` explicitly preserves that sink's native
callbacks and filtering. `.try_install()` returns `Installed` or
`AlreadyInstalled`; it never replaces an installed logger or lowers the global
maximum. Static `Record::args().as_str()` text is preserved; dynamic arguments use
an unavailable message. The log facade has no universal native error field.
`detach_logging()` stops SDK telemetry while leaving the application's logger in
place. Select one SDK sink for a bridged log/tracing path.

`ExceptionOccurrence::new(&snapshot)` creates an exact SDK occurrence token.
`with_error_log(&occurrence, context, synchronous_logging_closure)` associates its
protected snapshot with one native error log. `capture_occurrence(&occurrence,
context)` can report the same actual failure in middleware. Pair suppression
requires successful source admission, the same live operation, the same client
and exact token. Receipts expire after thirty seconds and are capped at 128.
Pairing is disabled for owner hooks or sampling because they may discard the only
event. Different operations and independent tokens remain independent reports,
subject to ordinary volume suppression. No pointer, message or timestamp pairing
is used. These closure helpers execute the application's requested synchronous
business operation; they are not deferred telemetry callbacks.

See the upstream [per-layer filtering](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/layer/trait.Filter.html)
and [typed field visitor](https://docs.rs/tracing/latest/tracing/field/trait.Visit.html)
contracts for their native composition semantics.

## Panic observation

`install_panic_observer()` explicitly installs a process dispatcher at application
bootstrap and returns a revocable `PanicObserver`. Keep the registration alive.
`is_active()` describes that client registration; it cannot prove that application
code has retained the process-global hook. Installation while already panicking,
duplicate client registration and exhausted dispatch capacity return inactive
registrations. Setup should be sequential with other application hook setup.

The dispatcher chains the previous hook exactly once and preserves native unwind
or termination behavior. Detaching only revokes the registration; it never
restores an old hook over a later application hook. If the application replaces
the dispatcher, observation stops until the application explicitly composes the
desired hooks. SDK-internal panics are not recaptured.

Only native string payloads and supplied locations are copied. Opaque payloads
are unavailable and remain owned by Rust's panic runtime/application. A location
is separate context, rather than a fabricated throw-site stack. Fatal additions
are bounded to 4 KiB, with no forced backtrace, application `before_send`, lazy
producer, ring flush, file write or network operation on the panic caller. Ordinary
queue/sampling/proof rules still apply. In abort mode queued telemetry may never
leave the process; no signal, OOM, crash persistence or next-launch replay is
claimed. See Rust's [native panic hook contract](https://doc.rust-lang.org/std/panic/fn.set_hook.html).

## HTTP middleware and trace propagation

Enable `axum` for `debugbundle::axum::DebugBundleLayer` (Axum 0.8), `tower` for
`debugbundle::tower::DebugBundleLayer` (Tower 0.5 / http 1 / http-body 1), or
`actix-web` for `debugbundle::actix_web::DebugBundleMiddleware` (Actix Web 4).
Actix retains its own native HTTP types and local non-Send application executor.
The generic Tower adapter likewise adds no Send requirement to application futures.
The outer SDK layer delegates readiness and calls the same inner service, following
the [Tower Service contract](https://docs.rs/tower/0.5.2/tower/trait.Service.html).

```rust,no_run
# #[cfg(feature = "axum")]
# async fn example() -> Result<(), Box<dyn std::error::Error>> {
use axum::{extract::Extension, routing::get, Router};
use debugbundle::{Client, Config, Context};
let client = Client::new(Config::new("server-project-token", "checkout"));
let app = Router::new().route("/checkout/{id}", get(
    |Extension(context): Extension<Context>| async move {
        // Context can be passed to capture, probes or an explicitly wrapped task.
        let _request_is_active = context.operation_active();
        "checkout"
    }
)).layer(debugbundle::axum::DebugBundleLayer::new(client.clone()));
let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
axum::serve(listener, app).await?;
client.shutdown_async(std::time::Duration::from_secs(2)).await;
# Ok(())
# }
```

For Actix, add `.wrap(DebugBundleMiddleware::new(client.clone()))` to the app.
Read `Context` from `HttpRequest::extensions()` using Actix's `HttpMessage` trait.
Axum supplies an `Extension<Context>`. Disabled clients or exhausted operation
capacity still supply an empty context so SDK state cannot cause extraction failure.
Scopes are entered only during the actual service call/future/body poll. Dropping
the request future or response body revokes the owner and its descendants.

Request events use the actual status and time to response headers. A 500 alone
does not establish a native exception; the middleware records only a request event.
Real service or body errors retain their native result and use the safe opaque
error path without invoking Display, Debug or source accessors. Application panics
propagate normally. Install the explicit panic observer separately if wanted.
Same-client nested layers reuse the existing request scope. The default relay
route, SDK ingestion/config paths and exact `HttpOptions::excluded_paths` are
excluded from automatic request incidents. Forwarded headers are never trusted
to select request identity or trace destinations.

Both adapters collect a bounded protected header map and matched route template
when available. Actix's app middleware may run before route matching; the completed
request uses the route reported by its native response request. The runtime cannot
universally discover the resolved framework version: set
`HttpOptions { framework_version: Some(actual_version), ..Default::default() }`
through `.with_options(...)` using your application's build metadata. Otherwise
the optional version is absent. Backend exceptions include available Rust compiler,
platform, architecture and process facts; unavailable allocator totals are omitted.

Body capture is off by default. An earlier application middleware may insert
`RequestBodySnapshot::new(&already_parsed_json)` into native request extensions.
The constructor rejects input over 8 KiB and protects owned primitives before
retaining them; client-specific privacy is reapplied during request admission.
The SDK never consumes, copies or buffers an arbitrary native request body stream.
Insert the snapshot before the SDK layer runs; this is not a JSON extractor that
reads the stream on the SDK's behalf.

Frameworks can convert handler errors into ordinary responses before outer
middleware sees them. Use Axum's `ErrorResponse::new(native_response, client,
occurrence, context)` or Actix's `ReportedError::new(native_error, client,
occurrence, context)` for explicit exception evidence. Construct the fallible
`ExceptionOccurrence::new(&safe_snapshot)` at the error boundary. These helpers
preserve native IntoResponse/ResponseError conversion. Reuse that exact occurrence
with `with_error_log` to pair an admitted native error log in the same operation;
matching text or a nearby timestamp never causes suppression. Actix delegates
Display/Debug only when the application/framework requests native error formatting,
not to build SDK telemetry. Hooks/sampling retain the documented pairing limits.

Middleware preserves a valid UUIDv4 `X-DebugBundle-Trace-Id` or generates a new one,
exposes it through context and adds it to the response. Operation IDs remain
separate. Set `Config::trace_propagation_targets` to explicit HTTP(S) origins, then
call `client.trace_header_for(destination, &context)`. Only a configured exact
origin receives a value; subdomain lookalikes, credential-bearing URLs and the
entire SDK ingestion origin are excluded. Add the returned value to your native
HTTP request without changing its body or other headers. Evaluate each redirect
destination again; disable automatic redirects that would carry custom headers
outside that decision. The helper does not wrap or reconfigure your HTTP client.
There is no automatic third-party or W3C trace substitution.

Both middleware adapters attach a safe `X-Request-Id` to request correlation.
An absent, oversized or privacy-sensitive value gets a fresh UUIDv4 instead;
it never becomes a shared redaction marker or discards the primary request event.
The request owner carries this correlation into its application captures.

Both middleware adapters consume `X-DebugBundle-Probe-Trigger` or `_debug_probe`
from the current request and bind a locally verified signed activation to its
request owner. A present invalid header never falls back to the query parameter.
The raw header is excluded from captured headers and the query value is protected
before metadata retention. Completion/drop revokes the activation; it never
enables a later request. Current server policy and signing-key rotation still apply.

## Browser relay candidate

The `local` feature exports the shared `relay::{Relay, RelayOptions, RelayRequest,
RelayResponse}` engine. `Relay::new(client, options)` inherits the client's
configuration and protected file transport; invalid options make it inactive.
`handle(&request_head, body_bytes).await` accepts the browser `{ "batch": [...] }`
wrapper with at most 100 members and 256 KiB. This explicit request handler may
await bounded persistence; it is separate from nonblocking application capture.
Eight in-flight ingress operations and 4,096 IP limiter keys per client bound
owned work across all relay instances for that client. Constructing another
framework worker/handler never resets these allowances. `axum::browser_relay(relay)` returns a Router to merge into the
application; `actix_web::browser_relay(relay)` returns a native service factory.
Both mount `/debugbundle/browser`, admit work before reading a body, limit
streamed bodies to 256 KiB/4,096 frames and end stalled reads after five seconds
with 408. Both adapters participate in the native and extracted-crate qualification matrix.

Axum plain listeners should use
`into_make_service_with_connect_info::<std::net::SocketAddr>()` to supply the
accepted socket peer. Actix reads its native peer and listener TLS configuration,
without using forwarded `connection_info()`. A custom listener may insert
`relay::RelayConnection { secure, peer }` from verified transport facts on either
framework. Never populate this extension from untrusted request headers.

`RelayOptions` supplies exact allowed origins, up to 16 exact trusted proxy peers,
the default 60/minute/IP limit, optional service/environment overrides and
`durable_write` (default true). Without an allowlist, validation compares the
Origin to the actual scheme/Host; an absent Origin may use Referer. A present
invalid Origin never falls back. Native adapters must supply the actual socket
peer and scheme. Forwarded host/proto/IP values apply only to a configured peer,
whose proxy must overwrite untrusted forwarding headers. Missing peers share one
conservative rate bucket. Configure explicit allowed frontend origins behind
reverse proxies unless their trust boundary has been qualified.

For replicas, `Config::relay_rate_limit_store` accepts an `Arc<dyn RateLimitStore>`
when `local` is enabled. `allow(RateLimitKey)` must atomically enforce a minute
window using the store's clock; keys contain a credential-scoped namespace digest,
verified peer (or the common missing-peer key) and configured limit. The SDK also
retains its bounded per-process limiter. Store callbacks run on the client's
existing worker before body reads, within `request_timeout`. Denial returns 429;
unavailability, panic or timeout returns 503. The client owns store cleanup, so
dropping a router cannot run an application store destructor on the caller.

The engine preserves browser timestamps and safe string/null correlations,
forces browser SDK identity, strips project/organization credentials and protects
known payloads before retention. Backend events, operation-scoped diagnostic
impersonation and invalid closed typed evidence are rejected. Analytics uses its
own generated receiving schema and semantic privacy checks; enabling browser
analytics and project capture remains a separate application decision.

The backend owner's `before_send` receives protected canonical events before
retention/forwarding, once per admitted event. Valid replacements may change any
schema-valid identity, then receive mandatory privacy and current reader proof
again. Invalid/panicking hooks use the safe original; a dropped or over-budget
replacement is withheld with indexed `local_policy` rejection. Worker failure or
timeout returns 503 without bypassing the hook. Persisted retries do not rerun it.

Allowed OPTIONS responses return 204 with POST/OPTIONS and
content-type/x-debugbundle-evidence CORS headers.
Allowed POST responses include the validated origin and Vary: Origin. Set the
head's `evidence` flag from `X-DebugBundle-Evidence: 1` or the
`debugbundle_evidence=1` query opt-in (automatic in native factories): indexed
errors then use 202 plus `evidence_errors` and a complete-path capability descriptor.
Legacy mixed batches preserve 400/string errors. Malformed wrappers use 400,
oversized bodies 413, forbidden origins 403 and rate limits 429. A busy, stopped
or unavailable delivery path returns 503 without a false acceptance receipt.

Local-only success means protected canonical arrays reached the private event
directory. Connected low-latency (`durable_write: false`) requires complete
canonical downstream acknowledgements, except the client's explicitly configured
legacy custom transport. Proof/policy piggyback shares the backend sender's trust
boundary, and every unsent byte-limited chunk rechecks proof.

Connected durable admission returns 202 only after immutable protected arrays
and authenticated per-index state are persisted in the private spool. One bounded
worker per client starts replay at relay construction, including after restart.
Scope binds the configured endpoint and a token digest; rotated credentials do
not replay old records. Metadata and completed markers use a private local HMAC
key. They contain no project token. A canonical indexed ACK is required even for
legacy custom transports. Partial ACKs retain only retryable indices; accepted
indices and their sent-event digests cannot be changed or retried. A record gains
`.delivered` only after all indices are canonically accepted. Terminal records
remain visible as pending until retention, rather than producing a delivery claim.
Known ACK progress keeps the same bounded record and file lease while callback
capacity or storage is unavailable. Persistence retries never resend those indices;
an identical write whose callback result was lost is idempotent. One stalled
persistence operation pauses that client's replay, without adding workers or
blocking request handlers. Flush and shutdown retain their finite deadlines.

`relay.status()` reports scan readiness, pending/delivered records and storage
failures for the current scope. `relay.flush_async(timeout).await` waits for
active ingress and current-scope replay within a maximum 30-second deadline;
false means the spool did not prove drained. SDK callback re-entry returns false.
Pending records expire after seven days from local admission; delivered records
remain for 24 hours from canonical completion. A private directory is bounded to
1,000 physical files and 100 MiB, including metadata/headroom. Corrupt, forged,
foreign or replaced files fail closed and cannot manufacture receipts. Native Linux, macOS and Windows source/extracted-crate tests cover restart, partial ACK, credential rotation and retention. Registry consumers remain a separate post-publication check.
Network acceptance followed by a crash or shutdown before ACK persistence may replay an ID;
this is at-least-once delivery, with canonical event IDs preserved for deduplication.

## Wire and local output

The optional `testkit` feature exposes `testkit::RecordingTransport`. Create it
with `new(capacity)` (1–1,000), retain an Arc in the test and supply it through
`Config::transport`. It validates/protects canonical events and emits real indexed
ACKs after bounded retention (eight MiB total). Invalid/full batches fail without
partial acceptance. `events()` returns a bounded protected snapshot; `drain()`
transfers retained events and releases their byte allowance. `fail_next(u8)` and
`attempts()` support deterministic transport failure/retry assertions. It installs
no globals and grants no reader proof implicitly. Use native HTTP mocks as well
when verifying an application's real ingestion or framework transport path.

`testkit::resources(&client)` returns a numeric `ResourceSnapshot`, or `None` for
an inactive client or contended state. It exposes current event count/bytes,
joint diagnostic bytes/windows/pending receipts, reserved operation slots/metadata
bytes and process client/callback-worker permits. It retains no payload or identity
and never waits for a lock or callback. Operation stop frees that owner's reservation
even while a caller keeps a revoked context; child reservations have their own
owners. Counters measure their declared allowances, not total heap allocation, and
process counters can advance independently of the client snapshot. The optional
testkit surface supports bounded ownership/performance tests without granting
reader proof or starting capture.

Built-in HTTP requires HTTPS, disables credential-bearing redirects and implicit
environment proxies, and bounds response bytes/time. Explicit local HTTP and
proxy configuration are opt-in. Final bodies include actual wrapper/commas under
256 KiB. Canonical indexed ACKs govern ownership; malformed ACKs retain the chunk.
Only the documented quota/rate-limit indexed rejections retry. Numeric/HTTP-date
retry hints cap at 300 seconds before deadline arithmetic.
JSON integer fields accept mathematically integral decimal/exponent forms, such
as `1.0`, under the same count/index/version bounds as Core. Fractional values and
invalid/out-of-range indices remain invalid. Reader feature pins accept any order
of unique known features and normalise them to the canonical descriptor order.

`evidence_capabilities` is an explicit assertion that the entire configured
delivery/relay/reader path supports the listed v1 features. It cannot override
server policy. Valid config/ACK discovery can revoke it; unsupported queued
primary additions are removed and context-only events are withheld. Unsent tails
are rechecked after every response without rerunning hooks.

Custom `delivery::Transport` runs on the bounded callback worker. Its default
requires canonical acknowledgements; only explicitly bodyless legacy transports
may opt into documented 2xx fallback with `requires_acknowledgement() == false`.
SDK-owned HTTP never uses that fallback.

Local-only output is protected canonical event arrays, private directories/files
and atomic writes. Linux/macOS source uses safe descriptor-relative OS APIs,
no-follow component traversal, effective-UID/private-mode checks and a kernel
no-replace rename so concurrent writers cannot overwrite a destination. Reads
require a private owned regular file with one link. The Windows implementation
retains ancestor handles without delete sharing, refuses reparse/UNC/device paths,
creates protected owner/System DACLs before writing, attests file ownership/ACLs
and single-link regular files, and uses flushed data plus write-through atomic
publication. The pinned directory ancestors cannot be renamed while retained.
Only `storage/windows/ffi.rs` permits SDK-owned unsafe Win32 ABI calls, with owned
buffers and RAII allocations/handles; application error traversal stays sealed and
unsafe-free. Windows cross-compilation is verified under Rust 1.89. Native CI executes ACL, filesystem, relay and coverage tests on Windows and macOS as well as Linux. Other local-file platforms fail closed.
