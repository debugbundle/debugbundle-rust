RUST_IMAGE ?= rust:1.99.1-bookworm@sha256:e6f2cff35d0f309acf8433b4f05878ed77092602c5065cc81801177da0bb3a9c
CURRENT_RUST_IMAGE = rust:1.100.0-bookworm@sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea
# Keep old compiler incremental state out of cross-toolchain qualification.
DOCKER_RUN = docker run --rm --init --ulimit core=0:0 -v "$(CURDIR):/work" -w /work -v debugbundle-rust-registry:/usr/local/cargo/registry -v debugbundle-rust-target:/target -e CARGO_TARGET_DIR=/target -e CARGO_INCREMENTAL=0
CARGO = $(DOCKER_RUN) $(RUST_IMAGE) cargo
CANDIDATE_ENV = -e DEBUGBUNDLE_CANDIDATE_ARTIFACT -e DEBUGBUNDLE_CANDIDATE_SHA256
TOOLS_IMAGE = debugbundle-rust-tools:1.99.1

.PHONY: toolchain
toolchain:
	docker build -f Dockerfile.test -t $(TOOLS_IMAGE) .

.PHONY: install build crate-info
.PHONY: test-release-tools
.PHONY: workflows-check
workflows-check:
	docker run --rm --init --network none -v "$(CURDIR):/repo:ro" -w /repo rhysd/actionlint:1.7.12 -color .github/workflows/ci.yml .github/workflows/release.yml
test-release-tools:
	python3 -m unittest discover -s scripts -p 'test_*.py'
install:
	$(CARGO) fetch --locked
build:
	$(CARGO) build --all-features --locked
.PHONY: examples example-local
examples:
	$(CARGO) check --examples --all-features --locked
example-local:
	$(CARGO) run --locked --example local --features local
crate-info:
	mkdir -p .tmp
	$(DOCKER_RUN) $(RUST_IMAGE) curl --proto '=https' --tlsv1.2 --fail-with-body --silent --show-error --write-out '%{http_code}\n' --output .tmp/cratesio-info.json https://crates.io/api/v1/crates/debugbundle

.PHONY: versions format format-check lint typecheck test test-contract test-frameworks test-relay test-msrv test-features package verify test-storage-faults
versions:
	$(DOCKER_RUN) $(RUST_IMAGE) rustc --version
format: toolchain
	$(DOCKER_RUN) $(TOOLS_IMAGE) cargo fmt --all
format-check: toolchain
	$(DOCKER_RUN) $(TOOLS_IMAGE) cargo fmt --all -- --check
lint: toolchain
	$(DOCKER_RUN) $(TOOLS_IMAGE) cargo clippy --all-targets --all-features --locked -- -D warnings
typecheck:
	$(CARGO) check --all-targets --all-features --locked
test:
	$(CARGO) test --all-features --locked $(TEST_ARGS)
.PHONY: test-core
test-core:
	$(CARGO) test --locked --no-default-features
test-contract:
	$(CARGO) test --all-features --locked --test privacy --test evidence --test analytics_wire
test-frameworks:
	$(CARGO) test --all-features --locked --test frameworks --test http_lifecycle --test remote_config --test relay_frameworks
test-relay:
	$(CARGO) test --all-features --locked --test relay --test relay_spool --test relay_frameworks --test relay_limiter
test-storage-faults:
	$(DOCKER_RUN) --network none --tmpfs /fault:rw,size=64k,mode=0700 -e DEBUGBUNDLE_STORAGE_FAULT_DIR=/fault/events $(RUST_IMAGE) cargo test --all-features --locked --offline --test storage_faults -- --ignored
test-msrv:
	$(MAKE) typecheck RUST_IMAGE=rust:1.89-bookworm@sha256:948f9b08a66e7fe01b03a98ef1c7568292e07ec2e4fe90d88c07bb14563c84ff
.PHONY: test-current
test-current:
	$(MAKE) test RUST_IMAGE=$(CURRENT_RUST_IMAGE)
test-features:
	$(CARGO) check --locked --no-default-features
	$(DOCKER_RUN) $(RUST_IMAGE) sh scripts/check-core-features.sh
	$(CARGO) check --locked --no-default-features --features local
	$(CARGO) check --locked --no-default-features --features tracing
	$(CARGO) check --locked --no-default-features --features log
	$(CARGO) check --locked --no-default-features --features tracing,log
	$(CARGO) check --locked --no-default-features --features tower
	$(CARGO) check --locked --features axum
	$(CARGO) check --locked --features actix-web
package:
	$(CARGO) package --locked --allow-dirty
	$(DOCKER_RUN) $(RUST_IMAGE) sh scripts/check-package-consumers.sh export
.PHONY: candidate smoke-package-lock smoke-package format-package-consumers compatibility-lock test-compatibility
candidate:
	@if [ -n "$$DEBUGBUNDLE_CANDIDATE_ARTIFACT" ]; then \
	  $(DOCKER_RUN) $(CANDIDATE_ENV) $(RUST_IMAGE) sh scripts/check-candidate.sh; \
	else $(MAKE) package; fi
smoke-package-lock: package
	$(DOCKER_RUN) $(RUST_IMAGE) sh scripts/check-package-consumers.sh lock
smoke-package: candidate
	$(DOCKER_RUN) $(CANDIDATE_ENV) $(RUST_IMAGE) sh scripts/check-package-consumers.sh verify
compatibility-lock: package
	$(DOCKER_RUN) $(RUST_IMAGE) sh scripts/check-package-consumers.sh lower-lock
test-compatibility: candidate
	$(DOCKER_RUN) $(CANDIDATE_ENV) $(RUST_IMAGE) sh scripts/check-package-consumers.sh lower
format-package-consumers: toolchain
	$(DOCKER_RUN) $(TOOLS_IMAGE) rustfmt --edition 2021 smoke/package-consumers/support/src/lib.rs smoke/package-consumers/vanilla/src/main.rs smoke/package-consumers/axum-app/src/main.rs smoke/package-consumers/actix-app/src/main.rs smoke/package-consumers/browser-host/src/main.rs
verify: format-check lint typecheck test test-features package

# Bootstrap resolution is explicit; subsequent commands use the frozen lockfile.
.PHONY: lock
lock:
	$(CARGO) generate-lockfile $(LOCK_ARGS)

.PHONY: panic-lock test-panic
panic-lock:
	$(CARGO) generate-lockfile --manifest-path smoke/panic-profiles/Cargo.toml
test-panic:
	$(DOCKER_RUN) $(RUST_IMAGE) sh scripts/check-panic-profiles.sh

.PHONY: coverage-toolchain coverage coverage-check
coverage-toolchain:
	docker build -f Dockerfile.coverage -t debugbundle-rust-coverage:1.99.1-0.9.1 .
coverage: coverage-toolchain
	mkdir -p .tmp/coverage
	$(DOCKER_RUN) -e CARGO_TARGET_DIR=/target/coverage debugbundle-rust-coverage:1.99.1-0.9.1 cargo llvm-cov --all-features --locked --lcov --output-path .tmp/coverage/lcov.info --ignore-filename-regex '/tests/|/tests\.rs$$|/examples/'
	$(MAKE) coverage-check
coverage-check:
	find src -type f -name '*.rs' ! -name tests.rs ! -path '*/tests/*' ! -path 'src/storage/windows.rs' ! -path 'src/storage/windows/*' | sort > .tmp/coverage/source-files.txt
	awk -f scripts/check-coverage.awk .tmp/coverage/source-files.txt .tmp/coverage/lcov.info

.PHONY: audit-toolchain audit native-typecheck native-test native-test-http
audit-toolchain:
	docker build -f Dockerfile.audit -t debugbundle-rust-audit:1.99.1-0.20.2 .
audit: audit-toolchain
	mkdir -p .tmp/audit
	$(DOCKER_RUN) -v debugbundle-rust-advisories:/usr/local/cargo/advisory-dbs debugbundle-rust-audit:1.99.1-0.20.2 cargo deny --locked check
	$(CARGO) tree --all-features --locked --format '{p} {l}' > .tmp/audit/dependencies.txt
.PHONY: audit-fuzz
audit-fuzz: audit-toolchain
	mkdir -p .tmp/audit
	cat deny.toml fuzz/license-allowance.toml > .tmp/audit/fuzz-policy.toml
	$(DOCKER_RUN) -v debugbundle-rust-advisories:/usr/local/cargo/advisory-dbs debugbundle-rust-audit:1.99.1-0.20.2 cargo deny --manifest-path fuzz/Cargo.toml --locked --config .tmp/audit/fuzz-policy.toml check
	$(CARGO) tree --manifest-path fuzz/Cargo.toml --locked --format '{p} {l}' > .tmp/audit/fuzz-dependencies.txt

# Only provisioned native CI runners use these targets. Local development keeps
# the pinned Docker targets; native runs establish the OS-specific semantics.
native-typecheck:
	python3 scripts/native-check.py typecheck
native-test:
	python3 scripts/native-check.py test
native-test-http:
	python3 scripts/native-check.py test-http

.PHONY: performance-lock evidence-performance performance-format performance-check
PERFORMANCE_REPORT ?= .tmp/performance/report.json
performance-format: toolchain
	$(DOCKER_RUN) $(TOOLS_IMAGE) cargo fmt --manifest-path smoke/performance/Cargo.toml
performance-lock:
	$(CARGO) generate-lockfile --manifest-path smoke/performance/Cargo.toml
evidence-performance:
	mkdir -p .tmp/performance
	$(CARGO) fetch --locked --manifest-path smoke/performance/Cargo.toml
	$(DOCKER_RUN) --cpus=2 --cpuset-cpus=0,1 --memory=2g -e CARGO_TARGET_DIR=/target/performance $(RUST_IMAGE) cargo run --release --locked --offline --manifest-path smoke/performance/Cargo.toml -- "$(PERFORMANCE_REPORT)"
	$(DOCKER_RUN) $(RUST_IMAGE) rustc -vV > .tmp/performance/toolchain.txt
	$(CARGO) metadata --locked --offline --format-version 1 --manifest-path smoke/performance/Cargo.toml > .tmp/performance/dependencies.json
performance-check:
	docker run --rm --init --network none -e PERFORMANCE_REPORT="$(PERFORMANCE_REPORT)" -v "$(CURDIR):/work:ro" -w /work $(BROWSER_NODE_IMAGE) node --test scripts/check-performance.test.mjs
	docker run --rm --init --network none -v "$(CURDIR):/work:ro" -w /work $(BROWSER_NODE_IMAGE) node scripts/check-performance.mjs "$(PERFORMANCE_REPORT)"

.PHONY: browser-lock browser-install browser-host test-browser-relays
BROWSER_NODE_IMAGE ?= node:24.14.0-bookworm@sha256:5a593d74b632d1c6f816457477b6819760e13624455d587eef0fa418c8d0777b
BROWSER_NODE = docker run --rm --init -v "$(CURDIR):/work" -w /work/smoke/browser-artifacts $(BROWSER_NODE_IMAGE)
.PHONY: crate-namespace
.PHONY: test-registry-tools smoke-published
.PHONY: release-preflight release-publish
RELEASE_ENV = $(CANDIDATE_ENV) -e RELEASE_VERSION -e RELEASE_SOURCE_SHA
release-preflight:
	$(DOCKER_RUN) $(RELEASE_ENV) $(RUST_IMAGE) sh scripts/release-candidate.sh preflight
release-publish:
	@$(DOCKER_RUN) $(RELEASE_ENV) -e CARGO_REGISTRIES_CRATES_IO_TOKEN $(RUST_IMAGE) sh scripts/release-candidate.sh publish
test-registry-tools:
	docker run --rm --init --network none -v "$(CURDIR):/work:ro" -w /work $(BROWSER_NODE_IMAGE) node --test scripts/check-published.test.mjs
smoke-published:
	docker run --rm --init -e RELEASE_VERSION -e RELEASE_SHA256 -v "$(CURDIR):/work" -w /work $(BROWSER_NODE_IMAGE) node scripts/check-published.mjs
	$(DOCKER_RUN) -e RELEASE_VERSION -e RELEASE_SHA256 $(RUST_IMAGE) sh scripts/check-published.sh prepare
	docker run --rm --init --network none -e RELEASE_VERSION -e RELEASE_SHA256 -v "$(CURDIR):/work:ro" -w /work $(BROWSER_NODE_IMAGE) node scripts/check-published.mjs consumer
	$(DOCKER_RUN) --network none -e RELEASE_VERSION -e RELEASE_SHA256 $(RUST_IMAGE) sh scripts/check-published.sh run
crate-namespace:
	docker run --rm --init -v "$(CURDIR):/work" -w /work $(BROWSER_NODE_IMAGE) node scripts/check-crate-namespace.mjs
browser-lock:
	$(BROWSER_NODE) npm install --package-lock-only --ignore-scripts --no-audit --no-fund --registry=https://registry.npmjs.org --userconfig=/dev/null
browser-install:
	$(BROWSER_NODE) npm ci --ignore-scripts --no-audit --no-fund --registry=https://registry.npmjs.org --userconfig=/dev/null
	$(BROWSER_NODE) node prepare.mjs
browser-host: candidate
	$(DOCKER_RUN) $(CANDIDATE_ENV) $(RUST_IMAGE) sh scripts/check-package-consumers.sh browser
test-browser-relays: browser-install browser-host
	docker run --rm --init --ulimit core=0:0 --network none --cpus=2 --memory=2g -v "$(CURDIR):/work:ro" -v "$(CURDIR)/.tmp/browser-qualification:/work/.tmp/browser-qualification" -w /work mcr.microsoft.com/playwright:v1.63.0-noble sh -lc 'set -- /ms-playwright/chromium-*/chrome-linux*/chrome; test "$$#" -eq 1 && CHROMIUM_PATH="$$1" node smoke/browser-artifacts/check.mjs'

.PHONY: windows-toolchain windows-typecheck windows-lint
windows-toolchain:
	docker build -f Dockerfile.windows -t debugbundle-rust-windows-check:1.89 .
windows-typecheck: windows-toolchain
	$(DOCKER_RUN) -e CARGO_TARGET_DIR=/target/windows-check debugbundle-rust-windows-check:1.89 cargo check --all-targets --no-default-features --features local,testkit,tracing,log --target x86_64-pc-windows-gnu --locked
windows-lint: windows-toolchain
	$(DOCKER_RUN) -e CARGO_TARGET_DIR=/target/windows-check debugbundle-rust-windows-check:1.89 cargo clippy --all-targets --no-default-features --features local,testkit,tracing,log --target x86_64-pc-windows-gnu --locked -- -D warnings

.PHONY: fuzz-toolchain fuzz-lock fuzz-format fuzz-info fuzz-build fuzz
fuzz-toolchain:
	docker build -f Dockerfile.fuzz -t debugbundle-rust-fuzz:2026-11-12-0.13.2 .
fuzz-lock:
	$(CARGO) generate-lockfile --manifest-path fuzz/Cargo.toml
fuzz-format: toolchain
	$(DOCKER_RUN) $(TOOLS_IMAGE) cargo fmt --manifest-path fuzz/Cargo.toml
fuzz-info: fuzz-toolchain
	$(DOCKER_RUN) debugbundle-rust-fuzz:2026-11-12-0.13.2 cargo +nightly-2026-11-12 fuzz build --help
fuzz-build: fuzz-toolchain
	$(DOCKER_RUN) debugbundle-rust-fuzz:2026-11-12-0.13.2 cargo +nightly-2026-11-12 fetch --manifest-path fuzz/Cargo.toml --locked
	$(DOCKER_RUN) --cpus=2 --memory=3g -e CARGO_BUILD_JOBS=1 -e CARGO_TARGET_DIR=/target/fuzz debugbundle-rust-fuzz:2026-11-12-0.13.2 sh scripts/check-fuzz.sh build
fuzz: fuzz-build
	python3 scripts/seed-fuzz.py
	$(DOCKER_RUN) --network none --cpus=2 --memory=2g -e CARGO_NET_OFFLINE=true -e CARGO_BUILD_JOBS=1 -e CARGO_TARGET_DIR=/target/fuzz debugbundle-rust-fuzz:2026-11-12-0.13.2 sh scripts/check-fuzz.sh run
