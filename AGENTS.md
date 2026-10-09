# Agent instructions

## Identity and scope

- **vosu** is the sole current name for the product, workspace packages, crates, executable, environment variables, data directories, release assets, updater metadata, first-party routes, public APIs, documentation, and operational tooling.
- Treat the rename as a repository-wide clean cutover. Do not add permanent aliases, compatibility packages, duplicate environment variables, fallback executable names, or new output containing the old brand.
- Preserve user state only through the narrowly scoped, one-time migration from existing **tosu** data and configuration locations. After migration, read and write only the **vosu** locations; migration code must remain idempotent and removable after the supported migration window.
- Branded public routes, payload names, and updater/release identifiers were intentionally included in the breaking rename. Third-party protocol names such as gosuMemory and streamCompanion remain unchanged. Upstream tosu identifiers may appear only in parity and provenance records.
- Do not blanket-replace vendored, generated, provenance-pinned, checksum-covered, upstream, or historical evidence. Change those artifacts only through their owning workflow; retain an old spelling only when it is an upstream identifier, historical fact, migration input, or compatibility fixture, and document the reason close to the allowlist or assertion.
- A branding change is complete only when case-sensitive and case-insensitive scans find no unexplained old-brand occurrences in live source, package metadata, lockfile package names, commands, configuration, CI, packaging, release paths, tests, or current documentation.
- This operational map describes the post-cutover tree. Any old spelling not explicitly identified as migration, upstream, parity, provenance, or history is a defect.
- This repository ships the native Rust daemon, not the former Electron/asdf-overlay application. Do not add Node, Electron, .NET, or C# runtime requirements to the daemon.
- Read [README.md](README.md) for the user-facing contract and [CONTRIBUTING.md](CONTRIBUTING.md) for the human setup path. Use the [architecture overview](docs/architecture/overview.md), [decision records](docs/decisions/index.md), and [benchmarks](docs/testing/benchmarks.md) for detail; keep this file operational.

## Toolchain and commands

Use the pinned Rust 1.96.1 toolchain from `rust-toolchain.toml`, including `rustfmt`, `clippy`, and both native targets. Windows builds require MSVC and the Windows SDK; Linux builds require a GNU C toolchain, Clang, pkg-config, and libelf, zlib, and libbpf development files for the process-event backends (per-distribution packages are in CONTRIBUTING.md). Keep `--locked`. Linux releases explicitly enable `linux-process-events`; Windows releases use no default features. GPU driver development libraries are not required.

```sh
# Local debug build/run
cargo build --locked
cargo run --locked

# CI checks
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --lib --bins --tests --examples --all-features --locked

# Native release build (select the host target)
cargo build --release --locked --no-default-features --features linux-process-events --target x86_64-unknown-linux-gnu
cargo build --release --locked --no-default-features --target x86_64-pc-windows-msvc

# Criterion hot paths; use the affected group or exact ID for scoped routine measurements.
# The unfiltered suite is available when measuring all hot paths; see docs/testing/benchmarks.md.
cargo bench --locked --package vosu --bench hot_paths -- <benchmark-filter>
```

The `test-harness` feature is development-only. `build.rs` intentionally rejects it in release builds; never treat it as a production switch.

Live source-versus-Rust parity is an interactive, attested gate, not a normal unit test. Use a strict JSON configuration accepted by the CLI, distinct loopback ports, and the pinned reference/client artifacts. Windows supports stable or lazer; Linux supports lazer. During capture, traverse menu → active play → paused → result, then close the client.

```sh
python tools/live-parity/run.py config-schema
python tools/live-parity/run.py check-config --config <run.json>
python tools/live-parity/run.py capture --config <run.json> --out <capture.json>
python tools/live-parity/run.py compare --capture <capture.json> --report <report.json>
python tools/live-parity/run.py diff --capture <capture.json> --out-dir <diff-directory>
```

## Source ownership

- `src/api/`: gosuMemory v1, streamCompanion, vosu v2, and precise payload projection/serialization.
- `src/server/` and `src/net/`: HTTP/WebSocket routing, files, commands, pages, connection handling, and streaming.
- `src/state/`, `src/instance/`, `src/process/`, `src/memory.rs`, `src/platform.rs`: process discovery, native memory reads, snapshots, and polling lifecycle.
- `src/config/`, `src/counters/`: watched configuration, migration, settings, counter discovery/download/storage.
- `src/updater/`: release selection, verified staging, replacement, rollback, and recovery.
- `src/calculation.rs` and `crates/vosu-pp/`: stable and lazer performance calculation; the latter is a Rust facade over provenance-pinned MIT source forks. The remaining source-provenance questions are recorded in `docs/provenance-audit.md`.
- `src/runtime/`, `src/app.rs`, `src/logging/`, `src/report/`: daemon orchestration and operational services.
- `crates/vosu-test-harness/`: integration-test support only. `tests/` holds compatibility, native, daemon smoke, and performance contracts. `crates/vosu-release-smoke/` exercises an externally packaged executable without depending on the daemon crate. `benches/hot_paths.rs` holds Criterion benchmarks.
- `tools/live-parity/`: deterministic live capture, attestation, comparison, and diff tooling. `tools/oracle/` holds calculator provenance/evidence tooling.

## Verification by change

Run the narrowest relevant command first; do not substitute a compile-only check for behavior verification.

- API projection or routing: the affected unit tests plus `cargo test --locked --test state_api_compat` and/or `cargo test --locked --test server_compat`; use `daemon_smoke` when startup or end-to-end serving changes.
- Configuration or counter persistence: affected unit tests plus `cargo test --locked --test config_compat` (and the relevant server compatibility test for counter APIs).
- Process discovery or memory reading: affected unit tests and, on each affected native OS, `cargo test --locked --test process_memory -- --nocapture`.
- Updater/release behavior: `cargo test --locked --test updater_compat`, native `cargo test --locked --test updater_native -- --nocapture`, and `VOSU_RELEASE_SMOKE_BINARY=<packaged-binary> cargo test --locked -p vosu-release-smoke --test release_smoke -- --ignored --nocapture` when packaging/startup changes.
- Calculator/state semantics: affected `vosu-pp` tests and `cargo test --locked --test performance_invariants`; use live parity when observable live schemas or state timing can change.
- Performance-sensitive hot paths: run the relevant Criterion target in release mode, compare like-for-like on the same machine, and record observations—not promises—in the [benchmark guide](docs/testing/benchmarks.md).
- Before merging a Rust change, run the applicable native smoke path and the CI checks above; platform-specific code requires evidence from that platform.

## Local CI and push gate

- Before pushing, read `.github/workflows/ci.yml` and run every applicable CI command locally against the final source being pushed. Use the pinned Rust toolchain and `--locked`; do not rely on hosted CI to discover failures after a push.
- Include change classification, maintained-document links, provenance/package fixtures, release-license tests, formatting, Linux workspace Clippy/tests, Windows MSVC cross-Clippy, and native Windows Clippy/tests whenever the workflow classifies the changes as native. Keep Linux-only header environment settings out of Windows cross-builds.
- Also run the full workspace test suite, including doctests (`cargo test --workspace --all-features --locked`), and affected tool suites and native smoke paths. A compile-only check or a narrowed test selection does not replace a required gate.
- Record the exact verified source/commit and each command's result. Existing evidence can cover unchanged bytes only when their identity is established; a historical pass from a different source generation is not a current pass.
- Push only after every required local check passes. A failed, interrupted, unavailable, or unexecuted check blocks the push. Fix the cause and rerun the affected gate; do not bypass hooks, weaken tests, silently skip platform checks, or push a partially verified tree.

## Compatibility and security invariants

- Preserve exact public routes, methods, status/error behavior, payload shapes, field semantics, and WebSocket upgrade paths unless an intentional compatibility decision updates tests and documentation. Unknown/near-miss routes must not become permissive.
- Preserve bounded request/frame handling and latest-state backpressure. Do not introduce unbounded queues or buffering in polling or streaming paths.
- Counter downloads must remain HTTPS-only in production, reject downgrade/private/reserved destinations, and retain redirect, ZIP path/symlink/count/expanded-size checks before same-filesystem replacement.
- Updater changes must retain platform-asset selection, published SHA-256 verification, single-root-executable staging, atomic replacement, rollback, and crash recovery.
- Keep report output escaped and configuration/settings writes same-directory and atomically replaced. Never weaken native process-memory permission boundaries or run the service as root to bypass them.
- Keep `#![forbid(unsafe_op_in_unsafe_fn)]`; isolate and justify platform `unsafe` at the native boundary.

## Vendored, generated, and documentation boundaries

- `assets/server/` and `assets/offsets.json` are vendored embedded inputs, not a frontend build workspace. Any byte change must preserve provenance and update `assets/SHA256SUMS`; update the matching `package.metadata.vosu` pins/hashes and [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) when their evidence changes.
- Treat `crates/vosu-pp/rosu-pp-api/` and `crates/vosu-pp/rosu-map-api/`, `tools/oracle/reference/`, oracle inputs, and calculator fixtures as provenance-pinned upstream evidence. Do not casually reformat or hand-normalize them; use the owning oracle/provenance tooling and preserve licenses, revision records, manifests, and source mappings.
- Do not hand-edit `Cargo.lock`; regenerate it with Cargo and review the dependency change. Recompute checksum manifests from the exact retained bytes and review the provenance change.
- Update README user contracts when commands, requirements, configuration, routes, installation, or runtime behavior change. Update architecture for structural boundaries, add an ADR for a durable decision, and update benchmark notes only with reproducible measurements.

## Maintained documentation

`docs/` is the maintained Markdown documentation root. Start with `docs/quickstart.md`. Update affected contracts and architecture in the implementation PR; preserve dated experiment and provenance records as historical evidence. Do not schedule inference, regenerate the whole tree on push, or add a documentation bot. Optional AI assistance must be explicitly scoped and reviewed.

Linux process-event backends are intentionally retained for their measured performance benefit. Preserve their default feature, capability-aware eBPF → CN_PROC → polling selection, bounded loss recovery, and periodic reconciliation; earlier experiment rollout cautions are historical, not instructions to remove the current backends.
