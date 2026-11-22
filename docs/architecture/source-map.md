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
| Native access | `crates/vosu-sys/` | Linux/Windows process handles, memory reads, paths, OS behavior, and process-exit-event backends; all platform `unsafe` lives here |
| osu! value types | `crates/vosu-osu-model/` | osu! memory/state/enum models and the calculation root helpers (JS-compatible rounding/number writing, mods, grades, legacy hits) shared by the reader, calculator, and adapters |
| Client decoding | `src/memory/`, `src/memory/stable/`, `src/memory/lazer/` | Offsets, signatures, pointers, snapshots, stable/lazer sentinels |
| State reduction | `src/state/` | Capture scheduling, stale-sample rejection, retention, gameplay state |
| Public APIs | `src/api/`, `src/model/api/` | v1, stream companion, v2, precise schemas and serialization |
| Server and transport | `src/server/`, `crates/vosu-net/` | Exact routes, auth, files, pages, WebSockets, commands, and hardened outbound streaming |
| Calculation | `src/calculation/`, `crates/vosu-pp/`, `crates/vosu-pp/rosu-map-api/`, `crates/vosu-pp/rosu-pp-api/` | Beatmaps, progressive state, live pp, legacy endpoint calculation, and provenance-pinned calculator APIs |
| Configuration and counters | `src/config/`, `src/counters/` | Canonical `vosu.toml`, one-time legacy dotenv migration, settings, discovery and downloads |
| Hosted tools | `src/tools/`, `assets/server/tool-bridge.js` | Manifest/discovery, official catalog installation, trust/settings persistence, child supervision, authenticated HTTP/WebSocket proxy and browser bridge; SDK in [vosu-tools](https://github.com/18-lab-dev/vosu-tools) |
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
`python scripts/check-file-length.py` enforces this in CI. Files that were already
over the limit are listed in `scripts/file-length-baseline.txt`; an entry may
only shrink or be removed (`--update` refuses to raise or add one).

Dependencies between the crate's top-level modules are recorded in
`scripts/module-boundaries.txt` and checked by
`python scripts/check-module-boundaries.py`. Adding an edge is a deliberate change:
record it in the same change that explains the new dependency here or in the
[architecture overview](overview.md). `memory`, `calculation`, and `state`
no longer cycle: the JS-compatible rounding/mods/grade helpers
(`crates/vosu-osu-model/src/root/{helpers,mods,scores}.rs`) and the state reducer's
calculation backend (`src/state/state_backend.rs`) live under `model` and
`state` respectively, not `calculation`, so readers depend only on the
model and `state -> calculation` (not the reverse) is the only edge left.
`server` and `tools` no longer cycle either: `server::routes::RouteExtension`
(implemented by `tools::ToolHost`) covers the reserved `/tools` overlay prefix
and the dashboard tool management routes, and `server::ids::valid_id`
(re-exported as `tools::manifest::valid_id`) is the shared identifier check,
so `tools -> server` is the only edge left.

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
  source of design tokens and `@font-face` rules (custom properties and font
  rules only, with absolute `/assets/` URLs); `system.css` holds the shared
  `v-` components. Tool UIs receive both, concatenated, as
  `/tools/_host/v1/theme.css`, and `design.html` is the reference page at
  `/tools/_host/v1/design` ([design guide](../tools/design.md)). `homepage-settings.js` owns the overlay and tool
  settings editor; `dashboard-tools.js` owns the Tools page and loads only
  there; `homepage-controls.css` follows the base stylesheet and preserves
  settings/dialog/responsive cascade order.

## Workspace boundaries

`Cargo.toml` defines one workspace with the production `vosu` package, the `vosu-sys` crate (native process/platform/process-event boundary, M0 crate split), the `vosu-net` crate (hardened outbound HTTPS streaming and archive extraction, M0 crate split), the `vosu-osu-model` crate (osu! value types and calculation root helpers, M0 crate split), the `vosu-pp` facade plus its `rosu-map-api` and `rosu-pp-api` members, the integration-only `vosu-test-harness`, the dependency-isolated `vosu-release-smoke` package, and the dev-only `vosu-proptest` package (`vosu_proptest_config()`, shared by every `prop_*` test). The smoke package launches an external packaged executable and does not depend on the daemon crate. Release profiles retain thin LTO, one codegen unit, stripped symbols, and abort-on-panic; `test-harness` is rejected by `build.rs` in release builds.

## Evidence hierarchy

Use `README.md` for user-facing behavior and prerequisites, the [architecture overview](overview.md) for current structural facts, [decision records](../decisions/index.md) for durable rationale, `AGENTS.md` for change verification and invariants, and tests/source for exact behavior. Vendored calculator code, oracle inputs, embedded assets, and checksum manifests are provenance-sensitive and should be changed only through their owning workflow.
