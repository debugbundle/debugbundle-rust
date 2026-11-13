# Rust application examples

These examples use the unpublished local candidate and compile in the frozen
SDK test matrix. They never require production canaries or a consumer checkout.
Use the matching CLI candidate for local processing; older published CLI builds
may require `project_id` and omit inline probes from local arrays.

| Example | Command inside the pinned development container | Behaviour |
| --- | --- | --- |
| `local` | `cargo run --locked --example local --features local` | Synchronous protected local error array, finite shutdown |
| `errors` | `cargo run --locked --example errors --features local` | Opaque anyhow/thiserror and explicit owned cause snapshots |
| `tokio` | `cargo run --locked --example tokio` | Current-thread executor, explicit task context and job owner |
| `tracing` | `cargo run --locked --example tracing --features local,tracing` | Composable native subscriber layer and primitive fields |
| `tower` | `cargo run --locked --example tower --features tower,local` | Generic Tower/http-body request and body-completion lifetime |
| `axum` | `cargo run --locked --example axum --features axum,tracing` | Native TCP middleware, matched route, full browser relay and graceful shutdown |
| `actix` | `cargo run --locked --example actix --features actix-web,tracing` | Native worker factories, middleware and full browser relay |

Routine host commands are `make examples` (compile all) and `make example-local`
(run the synchronous local example through Docker). Local output goes to
`.debugbundle/local/events`. Run the matching CLI's `debugbundle process --json`
there, then inspect its local incidents/bundles. Local files/durable relay require
qualified private filesystem support; current source supports Linux/macOS and
other platforms fail closed.

Connected examples read only the four named SDK environment variables described
in [configuration](../docs/configuration.md). Set a synthetic test endpoint and
server project token for qualification, or your explicitly selected environment
for application use. The two server examples listen on `127.0.0.1:3000` and expose
`GET /failure/{id}` as a synthetic 500 plus `/debugbundle/browser`. The failure
response is a request event, not an invented native exception. Supply your
application's safe error snapshots at real error boundaries.

Install logger/panic hooks only through explicit application composition. Default
capture still requires reader proof before optional evidence; the examples do not
assert support on behalf of an unknown receiving path. Browser credentials stay
out of the relay request. Configure exact frontend origins and trusted proxy peers
for your selected deployment, keeping the relay route outside member-token/CSRF
middleware. The SDK's own native HTTP is HTTPS-only unless explicitly opted into
development HTTP through Config.
