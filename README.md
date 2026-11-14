# DebugBundle for Rust

Safe, bounded runtime error reporting for vanilla Rust, Tokio, Axum and Actix Web.

The SDK supports Rust 1.89+, Tokio, Tower 0.5, Axum 0.8 and Actix Web 4, with
composable `tracing` and `log` integrations and browser relays. Source and exact
crate qualification covers Linux x64/arm64, macOS and Windows. See
[qualification](docs/qualification.md) for the release gates and
[crates.io](https://crates.io/crates/debugbundle) for published versions.

Every later SDK source/artifact requires its own qualification. `make package`
prepares a local candidate; `make smoke-package` tests its extracted contents.
Version tags start the automated [release workflow](docs/releasing.md).

## First local event

Add the SDK with explicit local-file support:

```toml
[dependencies]
debugbundle = { version = "1.0.0", default-features = false, features = ["local"] }
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

## Features and integrations

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
