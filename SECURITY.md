# Security policy

Report potential vulnerabilities privately through the
[DebugBundle security advisory flow](https://github.com/debugbundle/debugbundle/security/advisories/new).
Do not open public issues for credential exposure, redaction failures, unsafe
file handling, relay bypasses or token-scope defects.

Include the SDK version or source revision, feature flags, operating system,
minimal synthetic reproduction and observed impact. Remove live tokens and
customer payloads. The main project's security process covers triage and
coordinated disclosure for this SDK.

The current Rust candidate is unreleased. Its platform/runtime and artifact
qualification gates are recorded in the core implementation tracker; there is
no published Rust version to recommend for production adoption yet.
