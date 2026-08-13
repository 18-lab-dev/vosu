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
- Updater/release: `updater_compat`, native `updater_native`, and `release_smoke` for package/startup changes.
- Calculation/state: `vosu-pp` tests, `performance_invariants`, and live parity when observable schemas or timing can change; for v2 stats, cover parsed/missing maps, modes, mods, clock rate, and catch timing-window semantics. The oracle-backed `live_contracts` and `oracle_parity` tests require the crate’s `oracle-debug` feature.
- Compatibility payloads: assert `hitErrors`/`hitErrorArray` object fields (`all`, `press`, `release`) across v1, stream companion, v2, precise, play, and tournament fixtures; include the lazer mania hold release-correction case.
- Hot paths: the affected Criterion group in `benches/hot_paths.rs`, compared on the same machine; record observations in the [benchmark guide](benchmarks.md), not as universal thresholds.

## CI baseline

The required checks in `AGENTS.md` and `.github/workflows/ci.yml` are:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --lib --bins --tests --examples --all-features --locked
cargo test --locked --test process_memory -- --nocapture
cargo test --locked --test updater_native -- --nocapture
```

CI covers Linux and Windows validation, production release builds, and rejection of the development-only `test-harness` feature in release mode. The tagged release workflow also packages, extracts, and smoke-tests exactly the Linux and Windows archives.

## Evidence workflows

`tools/live-parity/` is an interactive attested compatibility gate, not a normal unit test. Validate configuration, capture a controlled client lifecycle, compare, and generate diffs. It requires pinned artifacts, distinct loopback ports, and supported platform/client combinations.

`tools/input-validation/` is a separate `uv` Python harness for precise keypress validation. Prefer dry-run map selection, navigation, autoplay, geometry, and live-autoplay before real input injection. Capture before the first press and after the final release. Treat source-reported `countDelta` as authoritative; sampled edge events are cadence-dependent and cannot alone prove losslessness.

## Configuration evidence

For configuration changes, inspect the effective platform config root's `vosu.toml` as a flat TOML document. Uppercase binding names must use TOML booleans, numbers, or quoted strings; for example, `SERVER_PORT = 24050.0` is a typed TOML number, not a dotenv assignment. Verify startup precedence `vosu.toml > vosu.env > tosu.env > defaults`, that canonical replacement completes before either readable legacy input is removed, and that malformed canonical input does not trigger a partial migration.

During watch reload, malformed UTF-8 or TOML must leave the last-good configuration and dependent resources active. A valid change is published only after `register_before_publish` work completes; failed listener replacement retains the previous listener. Programmatic writes should be debounced and use a same-directory temporary file plus atomic replacement, with a failed replacement leaving the current target intact.

Criterion benchmarks cover scanner/snapshot reads, beatmap parsing/calculation/cache behavior, serializers, publication fan-out, and selected-field subscriptions. They use synthetic fixtures and do not measure real process syscalls, browsers, network backpressure, or full snapshot construction.

## Test layout

`tests/` contains compatibility (`state_api_compat`, `server_compat`, `config_compat`, `updater_compat`), native (`process_memory`, `updater_native`), smoke (`daemon_smoke`, `release_smoke`), and invariant (`performance_invariants`) contracts. Focused unit/service tests live beside their owning modules. `crates/vosu-test-harness/` supports integration tests only.

When a change crosses [runtime boundaries](../architecture/overview.md), run the narrowest affected contract first and then the platform/live gate appropriate to the observable behavior. Do not substitute a compile-only check for behavior verification.
