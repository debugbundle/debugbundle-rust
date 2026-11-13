# Configuration reference

This is the unpublished `1.0.0-alpha.1` candidate. Construct a `Config`, override
fields with Rust struct update syntax, then call `Client::new`. Invalid enabled
configuration produces an inactive client with a fixed safe `status().reason`;
capture remains harmless. `Config::validate()` returns the typed validation error
for applications that want to check bootstrap configuration explicitly.

| Field | Default | Constraint and purpose |
| --- | --- | --- |
| `project_token` | empty | Required in connected mode; at most 4,096 bytes, no whitespace/control bytes. Server-side write credential only. |
| `endpoint` | `https://api.debugbundle.com/v1/events` | Absolute HTTPS URL, at most 8,192 bytes; no userinfo, fragment or privacy-sensitive path/query. SDK derives authenticated config discovery from this endpoint. |
| `service` | `rust-service` | Nonempty technical identity, at most 128 bytes; no controls or credentials. Canonical identity is independent of safe local filename labels. |
| `environment` | `development` | Same identity constraints as service. |
| `enabled` | `true` | False creates an inactive client without starting workers or validating unused fields. |
| `project_mode` | `ProjectMode::Connected` | `LocalOnly` requires `local` and rejects a custom network transport. |
| `local_events_dir` | `.debugbundle/local/events` | Private owned directory for local arrays. Unsafe paths, links, ownership or permissions fail closed. |
| `spool_dir` | `.debugbundle/local/browser-relay-spool` | Private owned durable browser relay directory. Separate from ordinary in-memory backend delivery. |
| `sample_rate` | `1.0` | Finite fraction in 0–1; zero captures no sampled events. |
| `log_level` | `Level::Warning` | Native SDK-layer threshold, independent of the application's other logging sinks. |
| `batch_size` | `25` | 1–100; final HTTP wrapper plus events also obeys 256 KiB. |
| `flush_interval` | 5 seconds | Greater than zero, at most 300 seconds. |
| `max_buffered_events` | `1000` | 1–1,000, including in-flight entries and unsent tails. |
| `max_buffered_bytes` | 8 MiB | 1 KiB–8 MiB, including in-flight entries and finalized tails. This is an event-byte allowance, not a total heap guarantee. |
| `max_event_age` | 10 minutes | Greater than zero, at most 10 minutes from original admission. Retries never refresh age. Diagnostics have their tighter five-minute association window. |
| `request_timeout` | 5 seconds | Greater than zero, at most 30 seconds. Whole-response deadline; flush uses its remaining common deadline. |
| `redact_fields` | empty | Additional field-name rules; cannot disable mandatory privacy. Invalid/over-budget rules fail validation. |
| `before_send` | `None` | `Arc<dyn Fn(Value) -> Option<Value> + Send + Sync>`; runs on the client's one bounded callback worker after initial privacy, followed by validation/privacy/policy again. `None` return drops the event. |
| `relay_rate_limit_store` | `None` | With `local`: optional shared `RateLimitStore`, called with bounded wait on the callback worker. Store failure fails relay admission closed. |
| `transport` | `None` | Optional owned custom `delivery::Transport`; default is native HTTP when enabled. Invoked on the callback worker. No HTTP feature requires an explicit transport in connected mode. |
| `allow_insecure_http` | `false` | Explicit development/self-hosted HTTP opt-in. Never enables redirects or ambient proxies. |
| `proxy` | `None` | Explicit reqwest proxy URL, at most 8,192 bytes; invalid proxy configuration makes native transport unavailable. Treat any configured proxy credentials as secrets. Environment proxy variables are ignored. |
| `capture_exception_evidence` | `true` | Allows safe snapshot evidence when complete-path proof and policy permit it. Does not traverse arbitrary Rust errors. |
| `capture_recovery_observations` | `false` | Explicit recovery opt-in, still subject to reader proof, diagnostics policy and the bounded recovery ledger. |
| `evidence_capabilities` | `None` | Explicit complete-path reader assertion `{ "version": 1, "features": [...] }`; only known unique feature names. Authenticated server discovery/revocation remains authoritative. |
| `max_probe_labels` | `50` | 1–50 protected ring labels per client. |
| `max_probe_entries_per_label` | `10` | 1–10 protected entries per label; combined ring data also has a 256 KiB cap. |
| `probe_flush_on_error` | `true` | Attach recent probes to an admitted exception when diagnostics are permitted. Clear only after successful retention. |
| `probes_poll_interval` | 60 seconds | 15–300 seconds. Server tier/directives control whether and how often automatic polling runs. Free/disabled discovery stops polling. |
| `trace_propagation_targets` | empty | At most 64 exact HTTP(S) origins. No credentials, paths, query or fragment. `trace_header_for` excludes the entire SDK ingestion origin. |

`Config::new(token, service)` applies the defaults above. `Config::local(service,
directory)` changes mode and local event directory. `Config::from_env()` reads
only `DEBUGBUNDLE_PROJECT_TOKEN`, `DEBUGBUNDLE_ENDPOINT`, `DEBUGBUNDLE_SERVICE` and
`DEBUGBUNDLE_ENVIRONMENT`; it does not enumerate environment variables, parse
unrelated application configuration or install global handlers.

Keep the client alive for the application lifetime. Select finite explicit
shutdown/flush deadlines. Capture methods never wait for the HTTP peer or invoke
application formatters/hooks on the capture caller. Native relay handling is an
explicit request handler that may wait for bounded private disk admission.

The native HTTP implementation uses reqwest with Rustls and native trust roots.
Its provider dependencies include native build tools; a C compiler and the normal
platform linker are needed when compiling from source. There is no advertised
alternate TLS backend in this candidate. The public dependency graph and MSRV
must be qualified together, using the frozen test/consumer resolutions.

Private local output/durable relay supports Linux, macOS and Windows, with native runtime and extracted-crate qualification on all three. Other platforms return `Unsupported` and never fall back to HTTP. See
[runtime boundaries](runtime-and-api.md) for callback, panic, logging, proof,
operation, recovery, relay and cleanup semantics.

HTTP uses explicit Rustls verification and requests identity-encoded responses.
Automatic gzip/Brotli/deflate/zstd response decompression is disabled on the SDK
client even when another dependency enables reqwest compression features;
compressed responses fail closed before JSON or indexed-ACK processing.
