# DebugBundle Rust SDK

Follow the core DebugBundle execution contract and `spec/sdks/rust-sdk.md` plus
`rust-sdk-qualification.md` in the core workspace. This repository is DebugBundle
only. Preserve host safety, mandatory privacy before retention, canonical wire
validation, bounded ownership and strict indexed acknowledgements.

Use the Docker-backed Make targets. No host package installation. Tests precede
behaviour changes. Keep source files below 800 lines and split before 1,000.
No commit, push, publication, deployment or consumer mutation without explicit
instructions for that action. Preserve unrelated work and continue when parallel
changes do not affect the current task or create an overwrite risk.
