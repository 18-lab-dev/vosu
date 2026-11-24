---
type: Verification Guide
title: vosu testing and verification
description: Test selection and evidence workflow for Rust unit/integration contracts, native process and updater checks, live parity, precise input validation, release smoke tests, and Criterion benchmarks.
resource: AGENTS.md
tags: [testing, ci, parity, benchmarks, release]
---

# Testing and verification

## Fast selection by change

- API projection/routing: affected tests plus `cargo test --locked --test state_api_compat` and/or `server_compat`; add `daemon_smoke` for startup/serving changes.
- Configuration/counters: affected unit tests plus `cargo test --locked --test config_compat`; cover flat typed TOML parsing, precedence `vosu.toml > vosu.env > tosu.env > defaults`, canonical replacement before legacy removal, malformed startup/reload preservation of last-good state, watcher handling, and atomic writes.
- Process/memory: affected tests plus `cargo test --locked --test process_memory -- --nocapture` on every affected native OS.
- Updater/release: `updater_compat`, native `updater_native`, and `VOSU_RELEASE_SMOKE_BINARY=<packaged-binary> cargo test --locked -p vosu-release-smoke --test release_smoke -- --ignored --nocapture` for package/startup changes.
- Calculation/state: `vosu-pp` tests, `performance_invariants`, and `state_api_compat`/`server_compat` when observable schemas or timing can change; for v2 stats, cover parsed/missing maps, modes, mods, clock rate, and catch timing-window semantics. The oracle-backed `live_contracts` and `oracle_parity` tests require the crate’s `oracle-debug` feature. Live parity is optional ([ADR 0011](../decisions/0011-live-parity-optional.md)).
- Compatibility payloads: assert the original `hitErrors`/`hitErrorArray` arrays plus additive sibling `pressHitErrors` and `releaseHitErrors` arrays across v1, stream companion, v2, precise, play, and tournament fixtures; include the lazer mania hold release-correction case.
- Untrusted-input and crash-recovery logic (ZIP preflight, updater recovery, API-state selection, served paths, JSON Pointers): `cargo test --workspace --lib --all-features --locked prop_`. These property tests use a fixed seed; `VOSU_PROPTEST_RANDOM=1 PROPTEST_CASES=<n>` explores new cases, and the nightly workflow runs 20,000 cases per property. A failure persists its seed to `proptest-regressions/lib-prop-failures.txt`; commit that file with the fix.
- Lazer offsets: `python scripts/lazer-offset-canary.py [--tag <release>-lazer]` stages the generator on the client's own runtime exactly as the daemon does and fails naming any template field it cannot resolve. The nightly workflow runs it against the newest lazer release.
- Dashboard and tool host: `tests/tool_host_smoke.py` and `tests/dashboard_smoke.py` against a `--features test-harness` debug build; CI's `dashboard` job runs them when UI paths change, and the nightly workflow also runs the catalog-art and tool-catalog smokes.
- Hot paths: the affected Criterion group in `benches/hot_paths.rs`, compared on the same machine; record observations in the [benchmark guide](benchmarks.md), not as universal thresholds.

## CI baseline

`.github/workflows/ci.yml` runs these checks; `scripts/prepush.py` runs the same set locally and in the Windows VM before `scripts/land.py` moves `main` ([ADR 0018](../decisions/0018-enforce-rules-with-checks.md)):

```sh
python scripts/check-doc-links.py
python scripts/check-file-length.py
python scripts/check-brand.py
python scripts/check-module-boundaries.py
python scripts/assemble-release-license.py
python scripts/check-asset-hashes.py
python -m unittest discover -s scripts -p 'test_*.py'
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo nextest run --workspace --lib --bins --tests --examples --all-features --locked
cargo xwin clippy --workspace --all-targets --all-features --locked --target x86_64-pc-windows-msvc -- -D warnings
# UI paths changed: build with --features test-harness, then
python tests/tool_host_smoke.py --binary target/debug/vosu
python tests/dashboard_smoke.py --binary target/debug/vosu --playwright-module <playwright/index.mjs>
# Native Windows (scripts/native-ci.ps1 in the VM):
cargo clippy --workspace --all-targets --all-features --locked --target x86_64-pc-windows-msvc -- -D warnings
cargo nextest run --workspace --lib --bins --tests --examples --all-features --locked --target x86_64-pc-windows-msvc
cargo test --workspace --doc --all-features --locked --target x86_64-pc-windows-msvc
```

CI runs on pull requests, on `main`, and on `ci/**` landing branches, and always publishes `CI required`, including for documentation-only changes. The classification job runs the repository check scripts without network or inference. Code changes run Linux Clippy/tests and a Windows MSVC cross-check. Shared, platform, build, or workflow changes also run native Windows verification; pure schema/calculator changes keep the cross-check without native Windows. Dashboard, tool host, and catalog paths add the `dashboard` smoke job. Manual dispatch runs every lane. The `main` ruleset requires `CI required`, with no bypass actors, rather than the conditionally skipped platform jobs. Cross-compilation is not native execution evidence. `scripts/test_gate_tooling.py` fails when `ci.yml` gains a check that the local gate does not run.

Locally, `prepush.py`'s `test-linux` step runs `cargo nextest run` inside a podman container built from `scripts/ci-container/Containerfile`, which mirrors `ci.yml`'s `ubuntu-latest` validation runner's package set (today Ubuntu 26.04; the Containerfile is the one place that release is named). The step builds a `cargo nextest archive` on the host, then replays it as a non-root user in the container with the source tree and `CARGO_TARGET_DIR` bind-mounted read-only at their own host paths, so compile-time-baked paths like `env!("CARGO_MANIFEST_DIR")`/`CARGO_BIN_EXE_*` still resolve and no workspace remap is needed. The image is tagged with the Containerfile's own content hash, so it rebuilds only when that file changes (a cold build adds roughly 10s; a warm one is instant) and the whole step adds under ~10s over a plain host `cargo nextest run` once the image is cached. This is what catches a failure specific to that distribution's packages before it reaches hosted CI — for example, ubuntu-latest's uutils-coreutils `sleep` refusing to run under a renamed executable name (run 45295358591), which a host run on a GNU-coreutils machine cannot see. Without `podman` on PATH the step falls back to a plain host run and records that fallback in the evidence; `scripts/test_ci_container.py` keeps the replayed command matched to ci.yml's and exercises the podman-unavailable/cargo-nextest-unavailable failure paths.

The nightly workflow (`.github/workflows/nightly.yml`) runs the lazer offset canary, the property tests with random seeds and 20,000 cases each, and all four browser smokes. Its failures reach the team through GitHub check-suite notifications.

MSVC SDK caching is independent of Cargo.lock and restricted to x86_64. Prepared SDKs are saved before later lint failures. Rust artifact caches use separate target/lane keys and weekly generations to limit archive churn; downloads are cached separately. The Ubuntu validation lane builds with `CARGO_PROFILE_DEV_DEBUG=line-tables-only`: full debuginfo made the all-target test build large enough to exhaust the hosted runner's disk. Tagged releases package, extract, and exercise the exact Linux/Windows archives, using a small smoke-test package that does not depend on the daemon crate.

## Evidence workflows

`tools/live-parity/` is an optional interactive diagnostic, not a normal unit test or a required check ([ADR 0011](../decisions/0011-live-parity-optional.md)). Use it to investigate a suspected divergence from upstream: validate configuration, capture a controlled client lifecycle, compare, and generate diffs. It requires pinned artifacts, distinct loopback ports, and supported platform/client combinations. Dated captures, release gates, and native runs are kept in the [verification history](verification-history.md) as evidence for their recorded source generations.

Native-reference mode also accepts the exact compressed upstream Linux 4.26.2
ELF (SHA-256 `fa22300bcdc88c3b18a11fd5ff70d7057530b4c4b9413a525eb92044951b034f`).
Its version string is not stored as plain ASCII. The digest is pinned in the
harness from the [published release archive](https://github.com/tosuapp/tosu/releases/download/v4.26.2/tosu-linux-v4.26.2.zip),
whose GitHub-published SHA-256 is
`6ee56962171dda360e62abed6fa52859cc64500cf3bdbb451a71b125cc452867`.
A configured digest alone cannot admit another compressed executable, and
ELF magic is still mandatory. This reference attests the published native
release, not a build of source revision `2359305`; report that distinction
with Linux capture evidence. Client identity, ownership, transport, lifecycle,
cadence, coverage, and comparison requirements are unchanged.


HTTP acquisition keeps one serial reusable connection for each daemon/schema
stream (eight streams total), drains every response, and rechecks the attested
process, listening socket, and exact established connection for every request.
Reusing a connection does not reuse its ownership observation: each sample must
have a fresh check beginning after the preceding sample and completed before
its own response timestamp. Failed requests remain failed samples and are not
replayed. Peer-announced closure permits a new connection on the next sample.
Each stream has a hard limit of 256 connection attempts; exceeding it fails the
capture rather than allowing unbounded churn. This bounds the collector to
2,048 HTTP connection attempts without changing the sample cadence, lifecycle
coverage, comparison tolerances, or OS TCP settings.

Capture metadata records per-stream `http_transport.connection_attempts` and
`request_attempts`. The durable HTTP endurance regression exercises 3,000 rounds
across all eight streams (24,000 responses), including length-framed, chunked,
and exact not-ready responses, with real native socket ownership observations.
It requires eight accepted connections and a fresh valid binding for every
sample, and rejects missing or reused ownership checks. Run it with
`python -m unittest discover -s tools/live-parity -p 'test_live_parity_http.py' -v`;
the full `test_*.py` suite also covers it.

`tools/input-validation/` is a separate `uv` Python harness for precise keypress validation. Prefer dry-run map selection, navigation, autoplay, geometry, and live-autoplay before real input injection. Capture before the first press and after the final release. Treat source-reported `countDelta` as authoritative; sampled edge events are cadence-dependent and cannot alone prove losslessness.

## Configuration evidence

For configuration changes, inspect the effective platform config root's `vosu.toml` as a flat TOML document. Uppercase binding names must use TOML booleans, numbers, or quoted strings; for example, `SERVER_PORT = 24050.0` is a typed TOML number, not a dotenv assignment. Verify startup precedence `vosu.toml > vosu.env > tosu.env > defaults`, that canonical replacement completes before either readable legacy input is removed, and that malformed canonical input does not trigger a partial migration.

During watch reload, malformed UTF-8 or TOML must leave the last-good configuration and dependent resources active. A valid change is published only after `register_before_publish` work completes; failed listener replacement retains the previous listener. Programmatic writes should be debounced and use a same-directory temporary file plus atomic replacement, with a failed replacement leaving the current target intact.

Criterion benchmarks cover scanner/snapshot reads, beatmap parsing/calculation/cache behavior, serializers, publication fan-out, and selected-field subscriptions. They use synthetic fixtures and do not measure real process syscalls, browsers, network backpressure, or full snapshot construction.

## Test layout

`tests/` contains compatibility (`state_api_compat`, `server_compat`, `config_compat`, `updater_compat`), native (`process_memory`, `updater_native`), daemon smoke (`daemon_smoke`), and invariant (`performance_invariants`) contracts. `crates/vosu-release-smoke/` tests an external packaged executable. Focused unit/service tests live beside their owning modules. `crates/vosu-test-harness/` supports integration tests only.

Keep unit tests for distinct consumer-visible behavior: boundaries, precedence, errors, state transitions, security, numerical results, and concurrency or resource bounds. Do not add tests that only repeat constants/help text, construct and read back fields, prove standard-library clone/ownership behavior, or echo arguments through mocks. When committed source-oracle fixtures already cover a wire contract, add a focused unit test only for an uncovered case; do not duplicate the same default or not-ready payload. Preserve provenance-pinned upstream tests through their owning workflow rather than culling them as first-party tests.

Filesystem fixtures must use names legal on both Windows and Linux. Build
expected native paths one component at a time with `Path::join`; reserve
literal `/` separators for URL or wire-format assertions. HTML-escaping tests
that create real directories can use `&`, rather than Windows-invalid `<`/`>`.

When a change crosses [runtime boundaries](../architecture/overview.md), run the narrowest affected contract first and then the platform/live gate appropriate to the observable behavior. Do not substitute a compile-only check for behavior verification.

