---
type: Source Map
title: vosu source map
description: Practical ownership map for navigating the vosu Rust workspace, tests, documentation, assets, and compatibility tooling.
resource: AGENTS.md
tags: [source-map, navigation, rust, ownership]
---

# Source map

Start with the owning boundary below rather than searching the whole tree. The [architecture overview](overview.md) explains how these areas connect at runtime.

| Area | Owner | Use it for |
|---|---|---|
| Startup and lifecycle | `src/app.rs`, `src/runtime/` | CLI decisions, composition, workers, shutdown, runtime reload |
| Desktop integration | `src/desktop/` | Session tray, executable ownership policy, opt-in Linux systemd user login service |
| Process instances | `src/instance/` | Candidate classification, focus, PID generations, tournament selection |
| Native access | `src/process/`, `src/platform/` | Linux/Windows process handles, memory reads, paths and OS behavior |
| Client decoding | `src/memory/`, `src/memory/stable/`, `src/memory/lazer/` | Offsets, signatures, pointers, snapshots, stable/lazer sentinels |
| State reduction | `src/state/` | Capture scheduling, stale-sample rejection, retention, gameplay state |
| Public APIs | `src/api/`, `src/model/api/` | v1, stream companion, v2, precise schemas and serialization |
| Server and transport | `src/server/`, `src/net/` | Exact routes, auth, files, pages, WebSockets, commands and streaming |
| Calculation | `src/calculation/`, `crates/vosu-pp/`, `crates/vosu-pp/rosu-map-api/`, `crates/vosu-pp/rosu-pp-api/` | Beatmaps, progressive state, live pp, legacy endpoint calculation, and provenance-pinned calculator APIs |
| Configuration and counters | `src/config/`, `src/counters/` | Canonical `vosu.toml`, one-time legacy dotenv migration, settings, discovery and downloads |
| Hosted tools | `src/tools/`, `assets/server/tool-bridge.js` | Manifest/discovery, official catalog installation, trust/settings persistence, child supervision, authenticated HTTP/WebSocket proxy and browser bridge; SDK in [vosu-tools](https://github.com/shiruviaa/vosu-tools) |
| Updating | `src/updater/` | Release selection, digest verification, staging, replacement, rollback |
| Reports and logging | `src/report/`, `src/logging/` | Report rendering, escaping, retention and operational diagnostics |
| Integration evidence | `tools/live-parity/`, `tools/oracle/` | Pinned parity captures, calculator provenance and diffs |
| Precise validation | `tools/input-validation/` | Map scanning, navigation, autoplay, edge accounting and capture artifacts |
| Behavioral contracts | `tests/`, `crates/vosu-test-harness/`, `crates/vosu-release-smoke/` | Compatibility, native, daemon smoke, updater/state/performance, and external packaged-executable tests |
| Performance evidence | `benches/hot_paths.rs`, [benchmark guide](../testing/benchmarks.md) | Criterion hot paths and machine-specific comparisons |
| Embedded inputs | `assets/`, `build.rs` | Dashboard, fonts, images, offsets and checksum-covered assets |

## Cleanup module boundaries

Maintained code files stay at or below 1,000 physical lines, including tests,
benchmarks, dashboard styles/scripts, and maintainer tools. Lockfiles and pinned
JSON/beatmap evidence are data, not source-size exceptions or splitting targets.

- `src/instance/runtime.rs` owns the discovery loop and lifecycle publication;
  `src/instance.rs` retains instance identity, manager state, and the public types.
- `src/state/bundle/publication.rs` owns immutable snapshot materialization and
  regular/precise Arc reuse; `reducer.rs` owns state transitions.
- `crates/vosu-pp/src/beatmap/random.rs` owns deterministic random-map transforms.
  Calculator tests group gradual, hit-window, mod, stateful-oracle, and scoring
  contracts under their existing integration-test entrypoints.
- Counter settings, v2 hit windows, pause transitions, and updater handoff tests
  live in child modules of their existing suites, sharing the existing fixtures.
- `examples/support/process_event_metrics.rs` owns resource and latency reporting,
  separate from the benchmark's process lifecycle orchestration.
- `tools/oracle/calculator/` separates binding helpers, provenance/coverage
  validation, replay contracts, and pinned multiplier/conversion evidence.
- `tools/input-validation/capture.py` owns bounded WebSocket transport and capture
  accounting; `input_validation.py` owns input playback and live scheduling.
  Both use `validation_error.py` for the shared error contract.
- Offset-generator partial classes separate runtime validation, target ownership,
  and generation; offset-target files separate assembly loading, schema, and CLI
  parsing. Their `bin/` and `obj/` trees are reproducible ignored build output.
  The prepared platform ZIP and checksum are release build inputs, while package
  locks remain maintained inputs.
- The dashboard loads `homepage.js` as an ES module, after the classic
  `theme-init.js`, which applies the stored colour mode before first paint. `tokens.css` is the single
  source of design tokens and `@font-face` rules; tool UIs receive the same file
  as `/tools/_host/v1/theme.css`, so it holds only `:root` and font rules with
  absolute `/assets/` URLs. `homepage-settings.js` owns the overlay and tool
  settings editor; `dashboard-tools.js` owns the Tools page and loads only
  there; `homepage-controls.css` follows the base stylesheet and preserves
  settings/dialog/responsive cascade order.

## Workspace boundaries

`Cargo.toml` defines one workspace with the production `vosu` package, the `vosu-pp` facade plus its `rosu-map-api` and `rosu-pp-api` members, the integration-only `vosu-test-harness`, and the dependency-isolated `vosu-release-smoke` package. The smoke package launches an external packaged executable and does not depend on the daemon crate. Release profiles retain thin LTO, one codegen unit, stripped symbols, and abort-on-panic; `test-harness` is rejected by `build.rs` in release builds.

## Evidence hierarchy

Use `README.md` for user-facing behavior and prerequisites, the [architecture overview](overview.md) for current structural facts, [decision records](../decisions/index.md) for durable rationale, `AGENTS.md` for change verification and invariants, and tests/source for exact behavior. Vendored calculator code, oracle inputs, embedded assets, and checksum manifests are provenance-sensitive and should be changed only through their owning workflow.
