# DebugBundle for Rust

Safe, bounded runtime error reporting for vanilla Rust, Tokio, Axum and Actix Web.

## Installation

```bash
cargo add debugbundle
```

Or add the dependency to `Cargo.toml`:

```toml
[dependencies]
debugbundle = "1.0.2"
```

Requires Rust 1.89 or newer. Enable framework and logging features only when using those integrations.

## Initialize

Run this in application startup and retain `client` for the application lifetime. Set `DEBUGBUNDLE_PROJECT_TOKEN` in the server environment. The example sets `environment` and `service` explicitly; `..Config::default()` supplies the remaining options. Initialization starts the client; panic observation and framework capture are explicit integrations. Flush only when verifying an event or during controlled shutdown.

```rust
use debugbundle::Config;

let client = debugbundle::init(Config {
    project_token: std::env::var("DEBUGBUNDLE_PROJECT_TOKEN").unwrap_or_default(),
    environment: "production".into(),
    service: "api".into(),
    ..Config::default()
});
```

## Integrations

Tokio, Tower, Axum and Actix Web, composable tracing/log integrations and browser relays are supported.

### Features and integrations

| Feature | Purpose |
| --- | --- |
| `http` (default) | Pooled Rustls HTTP with native trust; optional explicit proxy |
| `local` | Private event files and durable browser relay storage |
| `tower` | Native Tower readiness/future/body lifecycle |
| `axum` | Axum middleware and browser relay; includes HTTP/local/Tower |
| `actix-web` | Native non-Send Actix middleware and browser relay; includes HTTP/local |
| `tracing` | Composable per-layer filtering and bounded primitive field capture |
| `log` | Composable logger with explicit tee/fallible global installation |
| `testkit` | Bounded protected recording transport with indexed ACKs |

Compose `client.tracing_layer()` into an existing subscriber, or use
`client.logger().tee(existing_sink)` with the `log` feature. Instrument Axum with
`debugbundle::axum::DebugBundleLayer::new(client.clone())`, or Actix with
`debugbundle::actix_web::DebugBundleMiddleware::new(client.clone())`.

Both native `browser_relay(relay)` factories mount `/debugbundle/browser` and
provide origin validation, bounded uploads, credential isolation and durable
connected admission. Configure explicit allowed origins for split hosts. Browser
service names such as `checkout-web` remain distinct from backend `checkout-api`
unless the relay explicitly overrides them. See [the API guide](docs/runtime-and-api.md)
for complete lifecycle, body opt-in, proxy/peer, spool and status semantics.

Keep the guard returned by `client.install_panic_observer()` alive if you opt into panic observation. Process aborts do not guarantee delivery.

## First-event verification

Use an explicit setup event in a development or staging application, then confirm the matching service and environment in a received event or incident. Initialization, a healthy status and an empty queue are not delivery receipts. Keep the application running long enough for delivery; use bounded flushing only in tests or controlled teardown, never in a request handler.

<a id="first-local-event"></a>

Add the SDK with explicit local-file support:

```toml
[dependencies]
debugbundle = { version = "1.0.2", default-features = false, features = ["local"] }
```

```rust
use debugbundle::{Client, Config, Context, ExceptionSnapshot};
use std::time::Duration;

fn main() {
    let client = Client::new(Config::local("checkout-api", ".debugbundle/local/events"));
    client.capture_exception(
        &ExceptionSnapshot::new("SyntheticFailure", "First local event"),
        &Context::default(),
    );
    let outcome = client.flush(Duration::from_secs(2));
    println!("Accepted local events: {}", outcome.accepted_events);
}
```

Run `debugbundle process` in that application to read its canonical local arrays.
Capture is best effort: if no event was accepted, check `client.status()` and retry
the explicit verification event. A drained queue alone is not a delivery receipt.
Private file and relay runtime tests run natively on Linux, macOS and Windows; a cross-compile alone does not qualify an OS backend.

For connected capture, retain the default `http` feature and use
`Client::new(Config::from_env())`. Set `DEBUGBUNDLE_PROJECT_TOKEN`,
`DEBUGBUNDLE_SERVICE`, `DEBUGBUNDLE_ENVIRONMENT` and optionally
`DEBUGBUNDLE_ENDPOINT`. Connected mode always uses configured HTTP, including
development. Missing/invalid credentials produce an inactive client visible in
`status()`. Capture and a drained queue alone do not establish cloud acceptance.

For connected delivery, confirm the event in DebugBundle and inspect its incident with the CLI. For local-file delivery, run `debugbundle process` followed by `debugbundle inspect <incident-id>`. A local file or mock endpoint receipt proves only that destination; it does not prove cloud acceptance.

## Configuration

Set the project token, environment and service explicitly for your application. Keep the service name stable across deploys and distinct from other components. Configure optional capture, transport, privacy and probe settings only when needed.

See the [Rust configuration and runtime guide](https://debugbundle.com/docs/sdks/rust) and the [universal SDK interface](https://debugbundle.com/docs/sdks/universal-interface) for supported options and runtime limits.
`Config::from_env()` reads `DEBUGBUNDLE_PROJECT_TOKEN`, `DEBUGBUNDLE_SERVICE`, `DEBUGBUNDLE_ENVIRONMENT` and optional `DEBUGBUNDLE_ENDPOINT`. Connected mode always uses HTTP; `Config::local` selects local files explicitly. See [the configuration reference](https://github.com/debugbundle/debugbundle-rust/blob/main/docs/configuration.md).

## Development

Use Docker-backed `make test`, `make typecheck`, `make lint`, `make test-features`
and `make package`. The minimum supported Rust version is 1.89; CI includes that compiler and the complete native/platform matrix. Current and previous stable
source lanes use Rust 1.100.0 and 1.99.1. No Rust installation on the host is required.

## Safety model

Capture calls use bounded owned data and mandatory credential protection.
Arbitrary error formatters and source accessors do not run during capture.
Complete delivery requires canonical indexed acknowledgements.

The current API and its runtime limits are documented in
[the API guide](docs/runtime-and-api.md) and [configuration reference](docs/configuration.md). Axum/Tower and Actix middleware have
source integration tests. Extracted-crate vanilla/Axum/Actix consumers run on Linux x64/arm64, macOS and Windows; genuine published Browser ordinary/beacon/keepalive relay paths run on Linux arm64. Audit refinements must pass those gates again. See the
[qualification guide](docs/qualification.md) and
[release procedure](docs/releasing.md) for exact artifact and registry gates.

Licensed under Apache-2.0.

## Runtime and release qualification

The SDK supports Rust 1.89+, Tokio, Tower 0.5, Axum 0.8 and Actix Web 4, with
composable `tracing` and `log` integrations and browser relays. Source and exact
crate qualification covers Linux x64/arm64, macOS and Windows. See
[qualification](docs/qualification.md) for the release gates and
[crates.io](https://crates.io/crates/debugbundle) for published versions.

Every later SDK source/artifact requires its own qualification. `make package`
prepares a local candidate; `make smoke-package` tests its extracted contents.
Version tags start the automated [release workflow](docs/releasing.md).

### Hook size limits (unreleased correction)

A valid `before_send` replacement keeps its primary fields when optional exception
evidence crosses the combined 32 KiB limit. The SDK removes that evidence and its
typed reference and records `context.evidence_limits`. Malformed evidence still
uses the protected-original fallback even when it also exceeds the byte limit.
Mandatory privacy and existing ownership/transport limits remain enforced.

Hook replacements beyond the bounded JSON validation limit are withheld before optional-field reduction. Unvalidated shape is not treated as valid optional evidence, and the SDK does not restore pre-hook content in this case. Normal valid 32 KiB evidence overflow still preserves the reduced replacement.
