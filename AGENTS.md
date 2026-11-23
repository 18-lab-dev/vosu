# Agent instructions

## Identity and scope

- **vosu** is the sole current name for the product, workspace packages, crates, executable, environment variables, data directories, release assets, updater metadata, first-party routes, public APIs, documentation, and operational tooling.
- Treat the rename as a repository-wide clean cutover. Do not add permanent aliases, compatibility packages, duplicate environment variables, fallback executable names, or new output containing the old brand.
- Preserve user state only through the narrowly scoped, one-time migration from existing **tosu** data and configuration locations. After migration, read and write only the **vosu** locations; migration code must remain idempotent and removable after the supported migration window.
- Branded public routes, payload names, and updater/release identifiers were intentionally included in the breaking rename. Third-party protocol names such as gosuMemory and streamCompanion remain unchanged. Upstream tosu identifiers may appear only in parity and provenance records.
- Do not blanket-replace vendored, generated, provenance-pinned, checksum-covered, upstream, or historical evidence. Change those artifacts only through their owning workflow; retain an old spelling only when it is an upstream identifier, historical fact, migration input, or compatibility fixture, and document the reason close to the allowlist or assertion.
- `python scripts/check-brand.py` enforces the cutover: every old-brand occurrence in a tracked file needs a `scripts/brand-allowlist.txt` entry with a category (upstream, migration, parity, provenance, history, third-party-catalog, fixture), a maximum count that only ratchets down, and a reason. An occurrence that fits none of those categories is a defect; fix it rather than allowlisting it.
- This repository ships the native Rust daemon, not the former Electron/asdf-overlay application. Do not add Node, Electron, .NET, or C# runtime requirements to the daemon.
- Read [README.md](README.md) for the user-facing contract and [CONTRIBUTING.md](CONTRIBUTING.md) for the human setup path. Use the [architecture overview](docs/architecture/overview.md), [decision records](docs/decisions/index.md), and [benchmarks](docs/testing/benchmarks.md) for detail; keep this file operational.

## Toolchain and commands

Use the pinned Rust 1.96.1 toolchain from `rust-toolchain.toml`, including `rustfmt`, `clippy`, and both native targets. Windows builds require MSVC and the Windows SDK; Linux builds require a GNU C toolchain, Clang, pkg-config, and libelf, zlib, and libbpf development files for the process-event backends (per-distribution packages are in CONTRIBUTING.md). `crates/vosu-plugin-api/tests/layout.rs` additionally needs a C compiler on `PATH` (`$CC`, `cc`, `clang`, or `gcc`) to verify `include/vosu_plugin.h` against the Rust ABI types; on Windows this means Clang specifically (not `cl.exe`, which the test never invokes), installed separately from the MSVC/Windows SDK workload above. Keep `--locked`. Linux releases explicitly enable `linux-process-events`; Windows releases use no default features. GPU driver development libraries are not required. Tests run through `cargo-nextest` 0.9.152 (pinned in CONTRIBUTING.md), which kills any individual test that runs past the bound in `.config/nextest.toml` and names it in its output; nextest does not run doctests, so those run separately with plain `cargo test --doc`.

```sh
# Local debug build/run
cargo build --locked
cargo run --locked

# CI checks
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo nextest run --workspace --lib --bins --tests --examples --all-features --locked
cargo test --workspace --doc --all-features --locked

# Native release build (select the host target)
cargo build --release --locked --no-default-features --features linux-process-events --target x86_64-unknown-linux-gnu
cargo build --release --locked --no-default-features --target x86_64-pc-windows-msvc

# Criterion hot paths; use the affected group or exact ID for scoped routine measurements.
# The unfiltered suite is available when measuring all hot paths; see docs/testing/benchmarks.md.
cargo bench --locked --package vosu --bench hot_paths -- <benchmark-filter>
```

The `test-harness` feature is development-only. `build.rs` intentionally rejects it in release builds; never treat it as a production switch.

Live source-versus-Rust parity is an optional interactive diagnostic, not a required check before merging, pushing, or releasing ([ADR 0011](docs/decisions/0011-live-parity-optional.md)). Run it only to investigate a suspected divergence from upstream. It needs a strict JSON configuration accepted by the CLI, distinct loopback ports, and the pinned reference/client artifacts. Windows supports stable or lazer; Linux supports lazer. During capture, traverse menu → active play → paused → result, then close the client.

```sh
python tools/live-parity/run.py config-schema
python tools/live-parity/run.py check-config --config <run.json>
python tools/live-parity/run.py capture --config <run.json> --out <capture.json>
python tools/live-parity/run.py compare --capture <capture.json> --report <report.json>
python tools/live-parity/run.py diff --capture <capture.json> --out-dir <diff-directory>
```

## Source ownership

- `src/api/`: gosuMemory v1, streamCompanion, vosu v2, and precise payload projection/serialization.
- `src/server/` and `crates/vosu-net/`: HTTP/WebSocket routing, files, commands, pages, connection handling, and hardened outbound streaming/archive extraction.
- `src/instance/`: process discovery, snapshots, and polling lifecycle.
- `crates/vosu-plugin-api/`: vosu 2.0 plugin ABI v1 — `repr(C)` types mirroring `include/vosu_plugin.h` field by field, flat hot-path payload validation, and CBOR message codecs (`docs/v2/plugin-abi.md` §1–§5, §9).
- `crates/vosu-osu-reader/`: client decoding — stable/lazer memory readers and offsets, the pointer-width managed-heap read layer (`ProcessHandle`, which delegates its `scan_*` methods to `vosu-sys`'s generic signature-scan primitive) over `vosu-sys`'s object-safe syscall boundary, and osu!-specific discovery inputs (client argv parser `StartupArgs`, Wine data-path resolution `resolve_osu_path`).
- `crates/vosu-sys/`: native process/platform boundary (process handles, memory reads, OS paths/capabilities), the generic signature-scan primitive (AOB scanning) and module enumeration (`enumerate_modules`, Windows loader table / Linux `/proc/{pid}/maps`), and process-exit-event backends (eBPF/CN_PROC/polling); all platform `unsafe` lives here.
- `crates/vosu-osu-model/`: osu! memory/state/enum value types and the calculation root helpers (JS-compatible rounding/number writing, mods, grades, legacy hits) shared by the reader, calculator, and `src/model/api/` adapters.
- `crates/vosu-osu-state/`: reducer, capture scheduling, and gameplay state — serialized transition reducer, per-instance raw capture workers, and the calculation backend (`state_backend.rs`, `vosu-pp`'s `LiveBeatmapEngine`/`LiveStateCalculationBackend`).
- `src/counters/`: watched configuration, migration, settings, counter discovery/download/storage.
- `crates/vosu-core/`: config, logging, diagnostics report generation, self-update (release selection, verified staging, replacement, rollback, and recovery), desktop integration (session tray, executable ownership policy, Linux systemd user login service), startup decisions/guards, and the plugin host (`src/plugin/`: the explicit `BUNDLED` array plus external `dlopen` registry, ABI version negotiation and descriptor checks, the §7.1 in-process trust decision, instance lifecycle and dispatch — one thread per instance or the shared `concurrent_dispatch` pool — the host vtable `send` entry routing plugin-to-host messages into a host-side handler trait, process matching (`matcher.rs`, §6.1), session lifecycle, `focus_priority` holding, and the Focus rule (`session.rs`/`focus.rs`, §6.2, `docs/v2/api-v1.md` "Focus rule"), memory read plans and signature scans routed through `PluginHost` (`host/read_plans.rs`/`host/scans.rs`, §6.3, §6.4, sharing one per-session cached process handle and ownership check, each delivering its result through the normal dispatch queue — a `VOSU_READ_PLAN_SYNC` read plan is the one exception, delivered on the submitting thread itself), and the M1 host services — settings and interim bridge B2 (`settings.rs`, §6.9, D19), rate-limited logging (`logging.rs`, §6.10), and timers (`timers.rs`, §6.11); `docs/v2/plugin-abi.md` §1, §2, §4, §5, §6.1, §6.2, §6.3, §6.4, §6.9, §6.10, §6.11, §7.1).
- `crates/vosu-osu-calc/` and `crates/vosu-pp/`: stable and lazer performance calculation; the latter is a Rust facade over provenance-pinned MIT source forks. The remaining source-provenance questions are recorded in `docs/provenance-audit.md`.
- `src/runtime/`: daemon orchestration and operational services composed over `vosu-core`.
- `src/tools/`: tool host — manifest validation, discovery and official catalog installation, supervision, `/tools/` proxy and access rules, host API, launch sessions, dashboard tool APIs. The contract is [docs/tools/protocol.md](docs/tools/protocol.md) (ADR 0017); change both together. The SDK and official tools live in [vosu-tools](https://github.com/18-lab-dev/vosu-tools).
- `crates/vosu-test-harness/`: integration-test support only. `tests/` holds compatibility, native, daemon smoke, and performance contracts. `crates/vosu-release-smoke/` exercises an externally packaged executable without depending on the daemon crate. `benches/hot_paths.rs` holds Criterion benchmarks.
- `crates/vosu-proptest/`: shared `vosu_proptest_config()` used by every `prop_*` test across the workspace.
- `tools/live-parity/`: deterministic live capture, attestation, comparison, and diff tooling. `tools/oracle/` holds calculator provenance/evidence tooling.
- Module dependencies between a crate's own top-level `src/` modules are recorded per crate in `scripts/module-boundaries/<crate>.txt` and enforced by `python scripts/check-module-boundaries.py`; the allowed dependency edges between workspace crates are recorded in `scripts/crate-boundaries.txt` and enforced by `python scripts/check-crate-boundaries.py` against `cargo metadata`. A new edge (of either kind) is a deliberate change made together with the [source map](docs/architecture/source-map.md) or architecture update. Code files stay at or below 1,000 lines; `python scripts/check-file-length.py` enforces this, and its baseline of older long files only shrinks.

## Rules enforced by lints and checks

Most rules below are enforced mechanically ([ADR 0018](docs/decisions/0018-enforce-rules-with-checks.md)). When a check fails, fix the cause it names. When you correct a mistake that a check could catch, add or extend the check instead of adding prose here.

- Lint exceptions are `#[expect(lint, reason = "...")]` with the real reason; `#[allow]` is rejected (`clippy::allow_attributes`). Every `unsafe` block needs a `// SAFETY:` comment stating the invariant (`clippy::undocumented_unsafe_blocks`).
- `clippy.toml` disallows direct file writes (`std::fs::write`, `File::create`, and their tokio equivalents), unbounded channels, and `std::thread::sleep`, with messages naming the replacement. Production exceptions need a per-site `#[expect(clippy::disallowed_methods, reason = "...")]` (or `disallowed_types`). Test code is exempt at the crate level (a `#![cfg_attr(test, expect(...))]`), a `#[cfg(test)]` module, or a `tests/`/`benches/` file. `python scripts/check-lint-expectations.py` enforces that every production-code site matches a `scripts/lint-expectations.txt` entry (path, lint, a ratcheting maximum count, category, reason), the same ratchet discipline as `scripts/brand-allowlist.txt`.
- Blocking work in async code goes through `tokio::task::spawn_blocking`; the sleep rule exists because blocking waits on async workers have shipped before.
- Property tests are named `prop_*`, use `vosu_proptest::vosu_proptest_config()`, and run with a fixed seed. `VOSU_PROPTEST_RANDOM=1 PROPTEST_CASES=<n>` explores with a random seed, which the nightly workflow does.
- `python scripts/check-deleted-assertions.py`, a required `prepush.py` step, diffs HEAD against the merge-base and fails if an `assert!`/`assert_eq!`/`assert_ne!`/`debug_assert*`/`prop_assert*` line was removed from test code (a `#[cfg(test)]` module, or a `tests/`, `tests.rs`, or `*_tests.rs` file) without the same assertion text reappearing elsewhere in the diff; a deliberate removal needs an `Assertions-Removed: <reason>` trailer on a commit in the range.

## Verification by change

Run the narrowest relevant command first; do not substitute a compile-only check for behavior verification.

- API projection or routing: the affected unit tests plus `cargo test --locked --test state_api_compat` and/or `cargo test --locked --test server_compat`; use `daemon_smoke` when startup or end-to-end serving changes.
- Configuration or counter persistence: affected unit tests plus `cargo test --locked --test config_compat` (and the relevant server compatibility test for counter APIs).
- Process discovery or memory reading: affected unit tests and, on each affected native OS, `cargo test --locked --test process_memory -- --nocapture`.
- Updater/release behavior: `cargo test --locked --test updater_compat`, native `cargo test --locked --test updater_native -- --nocapture`, and `VOSU_RELEASE_SMOKE_BINARY=<packaged-binary> cargo test --locked -p vosu-release-smoke --test release_smoke -- --ignored --nocapture` when packaging/startup changes.
- Calculator/state semantics: affected `vosu-pp` tests and `cargo test --locked --test performance_invariants`, plus `state_api_compat`/`server_compat` when observable schemas or state timing can change.
- ZIP preflight, updater crash recovery, API-state selection, served-path validation, and JSON Pointer parsing: also `cargo test --workspace --lib --all-features --locked prop_`.
- Lazer offset generation or `tools/lazer-offset-*`: `python scripts/lazer-offset-canary.py [--tag <release>-lazer]` (needs the .NET 8 SDK) against the newest or the named lazer release; the nightly workflow runs it daily.
- Dashboard, tool host, or catalog UI (`assets/server/`, `src/server/`, `src/tools/`, `src/runtime/render*`): `python tests/tool_host_smoke.py --binary target/debug/vosu` and `python tests/dashboard_smoke.py --binary target/debug/vosu --playwright-module <playwright index.mjs>` against a `cargo build --locked --features test-harness` binary; CI's `dashboard` job runs both.
- Performance-sensitive hot paths: run the relevant Criterion target in release mode, compare like-for-like on the same machine, and record observations—not promises—in the [benchmark guide](docs/testing/benchmarks.md). The guide holds only each benchmark's latest result: replace the old section rather than appending a dated one; Git history keeps earlier results.
- Before merging a Rust change, run the applicable native smoke path and the CI checks above; platform-specific code requires evidence from that platform.
- Windows-sensitive changes (conditional compilation, path/line-ending handling, native process/memory code): run `python scripts/vm-test.py [--package P] [--test T] [--lib] [-- <filter>]` before handing off. It stages HEAD plus your uncommitted changes and runs a filtered `cargo test` natively in the Windows VM in minutes, well before `scripts/prepush.py`'s full native gate; it never starts the VM and never replaces that gate.
- `crates/vosu-sys/src/events/bpf/` (the comm-hint eBPF filter): run `python scripts/ebpf-verify.py` in the Linux VM. It stages HEAD plus your uncommitted changes into a unique guest directory, compiles `process_events.bpf.o` there, and loads every program with `bpftool -d prog loadall` to check each one's processed-instruction count and verifier states against a budget -- static `llvm-objdump` counts and "no loops" reasoning have both previously missed a real verifier rejection this catches. It never starts the VM.

## Local CI and landing

`main` requires the `CI required` status check, and no actor can bypass it. GitHub rejects a push of a commit that has not passed CI. Land every change this way:

```sh
python scripts/prepush.py [--start-vm] [--playwright-module <path>/playwright/index.mjs]
python scripts/land.py [--topic <name>]
```

- `prepush.py` refuses a dirty tree. It runs every applicable `ci.yml` check against `HEAD`:
  - formatting, Linux Clippy and the full workspace tests (unit/integration tests through `cargo nextest run`, doctests through a separate `cargo test --doc`) — a test that runs past `.config/nextest.toml`'s `slow-timeout` bound is killed and reported by name instead of hanging the gate;
  - the repository check scripts and every `scripts/test_*.py` suite;
  - actionlint when workflows changed;
  - Windows MSVC cross-Clippy;
  - the dashboard smokes when UI paths changed;
  - native Windows Clippy, nextest tests, and doctests in the VM through `scripts/native-ci.ps1`, but only when `scripts/windows_sensitivity.py` finds the diff against `--base` Windows-sensitive (the native process/platform boundary, updater staging, process/platform-specific modules, the plugin host's native-library loading, Cargo/toolchain manifests, `.cargo/`, `.github/workflows/`, `scripts/native-ci.ps1`/`prepush.py` itself, any `windows`/`win32`-named or `.ps1` path, or an added/removed `cfg(windows)`/`target_os`/`windows_sys`/`unsafe` diff line) or `--force-native` is passed; otherwise it records `not_applicable` with a reason and skips the VM entirely. Hosted CI's `windows-native` job still runs for every Rust change regardless, so this only narrows the slower local VM phase.

  It writes `target/prepush/<sha>.json`. `--start-vm` starts the Windows VM only if it is stopped and the native phase is applicable; report that lifecycle action.
- `land.py` accepts the evidence only when every step passed, or was not applicable, for exactly `HEAD`, and `HEAD` fast-forwards `origin/main`. It then pushes `ci/<topic>`, waits for hosted CI on that commit, fast-forwards `main`, and deletes the branch. Pass `--base <branch>` (and the matching `prepush.py --base <branch>`) to land onto a branch other than `main`, e.g. `v2/main`.
- `--skip` is for local iteration only; `land.py` refuses evidence with a skipped or failed required step. Fix the cause and rerun. Do not weaken tests or bypass the gate.
- When `ci.yml` gains a check, add it to `prepush.py` or `native-ci.ps1` in the same change. `scripts/test_gate_tooling.py` fails until the local gate covers it.
- Hosted cache saves for Cargo downloads and `target` artifacts run only from `main` (caches are branch-scoped and only `main`'s are readable cross-branch); `ci/<topic>` and pull-request runs restore only. A `main` push whose exact SHA already passed `CI required` on its `ci/<topic>` run (checked by `scripts/ci-check-verified.py`) skips its own lint/test steps and, on a cache miss, only rebuilds without testing to refresh the cache.

## Compatibility and security invariants

- Preserve exact public routes, methods, status/error behavior, payload shapes, field semantics, and WebSocket upgrade paths unless an intentional compatibility decision updates tests and documentation. Unknown/near-miss routes must not become permissive.
- Preserve bounded request/frame handling and latest-state backpressure. Do not introduce unbounded queues or buffering in polling or streaming paths.
- Counter downloads must remain HTTPS-only in production, reject downgrade/private/reserved destinations, and retain redirect, ZIP path/symlink/count/expanded-size checks before same-filesystem replacement.
- Updater changes must retain platform-asset selection, published SHA-256 verification, single-root-executable staging, atomic replacement, rollback, and crash recovery.
- Keep report output escaped and configuration/settings writes same-directory and atomically replaced; a reader of a file published through `vosu_sys::platform::atomic_replace` goes through the retrying read helper (`read_published`/`read_published_async`), is serialized against that file's own in-process writer by a lock the writer already holds while it publishes (e.g. counter settings files and `PROFILE_MUTATIONS`), or is a documented exclusion — an in-code comment at the read site, or the central list next to `read_published` (e.g. a same-task read strictly after that task's own publish, with no other writer of that path) — not a raw read. Files published through tempfile `persist` (a different, non-cross-directory publish) are out of scope for this helper. Never weaken native process-memory permission boundaries or run the service as root to bypass them.
- Keep `#![forbid(unsafe_op_in_unsafe_fn)]`; isolate and justify platform `unsafe` at the native boundary.

## Vendored, generated, and documentation boundaries

- `assets/server/` and `assets/offsets.json` are embedded inputs, not a frontend build workspace. Any byte change to a file listed in `assets/SHA256SUMS` must be reviewed and its digest refreshed with `python scripts/check-asset-hashes.py --write`. When regenerating `assets/offsets.json`, also record its provenance in `package.metadata.vosu` and [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) as described in the [offsets procedure](docs/operations/offsets-asset.md).
- Brand rasters (`assets/branding/icons/`, the social-preview PNG, `assets/icon.ico`, `assets/server/favicon.ico`) are generated from the SVG masters in `assets/branding/` by `python scripts/render-brand.py` (needs resvg); never edit them by hand. Rules are in [branding](docs/branding.md).
- Treat `crates/vosu-pp/rosu-pp-api/` and `crates/vosu-pp/rosu-map-api/`, `tools/oracle/reference/`, oracle inputs, and calculator fixtures as provenance-pinned upstream evidence. Do not casually reformat or hand-normalize them; use the owning oracle/provenance tooling and preserve licenses, revision records, manifests, and source mappings.
- Do not hand-edit `Cargo.lock`; regenerate it with Cargo and review the dependency change. When `crates/vosu-pp/THIRD_PARTY_LICENSES.txt` changes, regenerate the release `LICENSE` with `python scripts/assemble-release-license.py --write`, then refresh its digest.
- Update README user contracts when commands, requirements, configuration, routes, installation, or runtime behavior change. Update architecture for structural boundaries, add an ADR for a durable decision, and update benchmark notes only with reproducible measurements.

## Maintained documentation

`docs/` is the maintained Markdown documentation root. Start with `docs/quickstart.md`. Update affected contracts and architecture in the implementation PR; preserve dated experiment and provenance records as historical evidence. Do not schedule inference, regenerate the whole tree on push, or add a documentation bot. Optional AI assistance must be explicitly scoped and reviewed.

Linux process-event backends are intentionally retained for their measured performance benefit. Preserve their default feature, capability-aware eBPF → CN_PROC → polling selection, bounded loss recovery, and periodic reconciliation; earlier experiment rollout cautions are historical, not instructions to remove the current backends.
