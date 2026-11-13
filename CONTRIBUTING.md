# Contributing

This repository contains the DebugBundle Rust SDK. It is an unpublished candidate;
passing source checks does not establish release readiness.

Use Docker and Make. `make format-check`, `make lint`, `make typecheck` and
`make test` run pinned tools without installing Rust on the host.
`make test-features` checks independent feature combinations; `make test-msrv`
checks Rust 1.89 and `make test-current` runs the current pinned stable compiler.
`make test-panic` exercises isolated unwind/abort consumers. `make package` builds
the crate locally and never publishes it.
See [qualification](docs/qualification.md) for extracted-artifact, native Browser,
coverage, dependency and instrumented performance gates and their limits.

Before changing behaviour, read `AGENTS.md`, the core SDK contracts and the Rust
implementation/qualification documents linked from the core repository. Add a
failing functional test, implement the smallest complete change and update the
public API guide. Preserve mandatory privacy, bounded count/byte ownership,
canonical indexed ACKs and native framework behaviour. Automatic capture must
not execute arbitrary application formatting, block a request or install global
logging/panic hooks implicitly.

Fixtures originate in the core DebugBundle repository and include provenance and
checksums under `tests/fixtures`. Update the canonical source and regenerate its
fixtures rather than weakening an SDK assertion. Keep functional source files
below the execution contract's size limits.

Do not include tokens, private events, consumer configuration or generated local
telemetry in issues or changes. Follow [the security policy](SECURITY.md) for
potential vulnerabilities. Commits, source push, package publication, production
actions and adoption by another product require their own authorisation.
