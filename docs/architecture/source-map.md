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
| Startup and lifecycle | `src/runtime/` | CLI decisions, composition, workers, shutdown, runtime reload, loading `plugins::BUNDLED` and `--plugin <dir>` development plugins through `vosu-core`'s plugin host at startup and destroying them at shutdown (`src/runtime/dev_plugins.rs`); starting `vosu-core`'s generic discovery service and (Windows) foreground poller once an enabled plugin has a `[match]` table, and stopping both before plugin destruction on every exit path (`src/runtime/discovery.rs`, M1 §4 phase d2b unit d) |
| Core services | `crates/vosu-core/` | Config, logging, diagnostics report generation, self-update, desktop integration (session tray, executable ownership policy, opt-in Linux systemd user login service), startup decisions/guards, and the plugin host (`crates/vosu-core/src/plugin/`: the `BUNDLED`/`dlopen` registry, instance lifecycle, dispatch, host vtable `send` routing, process matching (`matcher.rs`, §6.1), session lifecycle/the Focus rule (`session.rs`/`focus.rs`, §6.2), the generic discovery service (`discovery.rs`, §6.1, M1 §4 phase d2b: one dedicated thread, a pluggable `ProcessSource` seam — Linux eBPF/CN_PROC/polling, Windows polling with an executable-name pre-filter against the enabled plugin set's `process_names`, and a Windows foreground-window poller), memory read plans and signature scans routed through `PluginHost` (`host/read_plans.rs`/`host/scans.rs`, §6.3/§6.4, sharing one per-session cached process handle), and the M1 host services settings/logging/timers (§6.9-§6.11, built on the core `config`/`logging` modules) — `docs/v2/plugin-abi.md` §1, §2, §4, §5, §6.1, §6.2, §6.3, §6.4, §7.1) |
| Process instances | `src/instance/` | Candidate classification, focus, PID generations, tournament selection |
| Plugin ABI v1 | `crates/vosu-plugin-api/` | `repr(C)` ABI v1 types mirroring `include/vosu_plugin.h` (constants, opcodes, flags, error codes, envelope, flat payload structs, host/plugin vtables), layout tests against `docs/v2/plugin-abi.md` §3, flat payload validation, CBOR codecs, and the `Plugin`/`ConcurrentPlugin` Rust SDK (`docs/v2/plugin-abi.md` §9) built on top of them |
| Surface plugin API | `crates/vosu-surface/` | Core-owned, permanent Rust-only API for bundled surface plugins: route/WebSocket-feed registration, `STATE_SUBSCRIBE`-equivalent state reads (D3, plan:94). Its `bridge_b1` module is interim: bridge B1's process-wide `LegacySink` hook slot (plan:447-452, abi:294 §7.5) feeding the gosuMemory/streamCompanion adapters, deleted along with the `vosu-osu -> vosu-surface` edge in M4a |
| Bundled plugins | `src/plugins.rs` | The explicit `BUNDLED` array (D13): every linked-in plugin's embedded manifest text and vtable-builder function, loaded by `src/runtime/dev_plugins.rs` at startup |
| Native access | `crates/vosu-sys/` | Linux/Windows process handles, memory reads, paths, OS behavior, and process-exit-event backends; all platform `unsafe` lives here |
| osu! value types | `crates/vosu-osu-model/` | osu! memory/state/enum models and the calculation root helpers (JS-compatible rounding/number writing, mods, grades, legacy hits) shared by the reader, calculator, and adapters |
| Client decoding | `crates/vosu-osu-reader/` | Stable/lazer memory readers and offsets, the pointer-width managed-heap read layer (`ProcessHandle`, AOB scanning), osu!-specific discovery inputs (client argv parser, Wine data-path resolution, classification predicates in `discovery::classify`), and the embedded self-contained lazer offset generator (`offset_generator/`, bundle embedded by this crate's own `build.rs`) |
| State reduction | `crates/vosu-osu-state/` | Capture scheduling, stale-sample rejection, retention, gameplay state |
| osu! capture and interim B1 | `crates/vosu-osu/src/capture.rs`, `bridge_b1.rs`, `session/capture.rs` | One shared reader preparation/publication implementation, used by the legacy supervisor until C3; plugin-owned reader tasks combine D19 settings with B1's latest control inputs. B1's publisher depends on capture's API/live cadence selection and settings; session capture owns both halves, without a `vosu-core` or `vosu-sys` edge. Session cancellation closes scan admission using the shared reader/backend liveness flag, so late or queued preparation cannot extend detach to the scan timeout. Offset installation wakes capture through a readiness watch; reader shutdown joins on the blocking pool. Stable retains the legacy resolving-offsets dashboard phase until its first publication. |
| Public APIs | `src/api/`, `src/model/api/` | v1, stream companion, v2, precise schemas and serialization |
| Server and transport | `src/server/`, `crates/vosu-net/` | Exact routes, auth, files, pages, WebSockets, commands, and hardened outbound streaming |
| Calculation | `crates/vosu-osu-calc/`, `crates/vosu-pp/`, `crates/vosu-pp/rosu-map-api/`, `crates/vosu-pp/rosu-pp-api/` | Beatmaps, progressive state, live pp, legacy endpoint calculation, and provenance-pinned calculator APIs |
| Counters | `src/counters/` | Settings, discovery and downloads |
| Hosted tools | `src/tools/`, `assets/server/tool-bridge.js` | Manifest/discovery, official catalog installation, trust/settings persistence, child supervision, authenticated HTTP/WebSocket proxy and browser bridge; SDK in [vosu-tools](https://github.com/18-lab-dev/vosu-tools) |
| Integration evidence | `tools/live-parity/`, `tools/oracle/` | Pinned parity captures, calculator provenance and diffs |
| Precise validation | `tools/input-validation/` | Map scanning, navigation, autoplay, edge accounting and capture artifacts |
| Behavioral contracts | `tests/`, `crates/vosu-test-harness/`, `crates/vosu-release-smoke/` | Compatibility, native, daemon smoke, updater/state/performance, and external packaged-executable tests |
| Performance evidence | `benches/hot_paths.rs`, [benchmark guide](../testing/benchmarks.md) | Criterion hot paths and machine-specific comparisons |
| Embedded inputs | `assets/`, `build.rs`, `crates/vosu-osu-reader/build.rs` | Dashboard, fonts, images, offsets and checksum-covered assets; the reader's own `build.rs` additionally embeds the RID-selected lazer offset generator bundle |

## Cleanup module boundaries

Maintained code files stay at or below 1,000 physical lines, including tests,
benchmarks, dashboard styles/scripts, and maintainer tools. Lockfiles and pinned
JSON/beatmap evidence are data, not source-size exceptions or splitting targets.
`python scripts/check-file-length.py` enforces this in CI. Files that were already
over the limit are listed in `scripts/file-length-baseline.txt`; an entry may
only shrink or be removed (`--update` refuses to raise or add one).

Dependencies between each crate's own top-level modules are recorded per
crate in `scripts/module-boundaries/<crate>.txt` and checked by
`python scripts/check-module-boundaries.py`; the allowed dependency edges
between workspace crates are recorded in `scripts/crate-boundaries.txt` and
checked by `python scripts/check-crate-boundaries.py` against `cargo
metadata`. Adding either kind of edge is a deliberate change: record it in
the same change that explains the new dependency here or in the
[architecture overview](overview.md). `memory`, `calculation`, and `state`
no longer cycle: the JS-compatible rounding/mods/grade helpers
(`crates/vosu-osu-model/src/root/{helpers,mods,scores}.rs`) and the state reducer's
calculation backend (`crates/vosu-osu-state/src/state_backend.rs`) live under
`vosu-osu-model` and `vosu-osu-state` respectively, not `vosu-osu-calc`, so readers
depend only on the model and `vosu-osu-state -> vosu-osu-calc` (a `scripts/crate-boundaries.txt`
crate edge, not a `scripts/module-boundaries/<crate>.txt` module edge, since `state` is no longer a top-level
module of the `vosu` package) is the only edge left.
`server` and `tools` no longer cycle either: `server::routes::RouteExtension`
(implemented by `tools::ToolHost`) covers the reserved `/tools` overlay prefix
and the dashboard tool management routes, and `server::ids::valid_id`
(re-exported as `tools::manifest::valid_id`) is the shared identifier check,
so `tools -> server` is the only edge left.

- `src/instance/runtime.rs` owns the discovery loop and lifecycle publication;
  `src/instance.rs` retains instance identity, manager state, and the public types.
- `crates/vosu-osu-reader/src/offset_generator.rs` depends on its own crate's
  `assets` (the embedded generator bundle and offsets template) and `memory`
  (`lazer_loader`'s `OffsetGenerator` trait it implements, `lazer_version`'s
  version check) module edges, recorded in
  `scripts/module-boundaries/vosu-osu-reader.txt`; no other module reaches it.
- `crates/vosu-osu-state/src/bundle/publication.rs` owns immutable snapshot materialization and
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

`Cargo.toml` defines one workspace with the production `vosu` package, the `vosu-sys` crate (native process/platform/process-event boundary, M0 crate split), the `vosu-net` crate (hardened outbound HTTPS streaming and archive extraction, M0 crate split), the `vosu-osu-model` crate (osu! value types and calculation root helpers, M0 crate split), the `vosu-osu-calc` crate (beatmap parsing, progressive state, and legacy/live calculation engines over `vosu-pp`, M0 crate split), the `vosu-osu-reader` crate (stable/lazer memory readers and offsets, the pointer-width managed-heap read layer, and osu!-specific discovery inputs, M0 crate split), the `vosu-osu-state` crate (reducer, capture scheduling, and gameplay state, plus the state calculation backend, M0 crate split), the `vosu-pp` facade plus its `rosu-map-api` and `rosu-pp-api` members, the integration-only `vosu-test-harness`, the dependency-isolated `vosu-release-smoke` package, and the dev-only `vosu-proptest` package (`vosu_proptest_config()`, shared by every `prop_*` test). The smoke package launches an external packaged executable and does not depend on the daemon crate. Release profiles retain thin LTO, one codegen unit, stripped symbols, and abort-on-panic; `test-harness` is rejected by `build.rs` in release builds.

## Evidence hierarchy

Use `README.md` for user-facing behavior and prerequisites, the [architecture overview](overview.md) for current structural facts, [decision records](../decisions/index.md) for durable rationale, `AGENTS.md` for change verification and invariants, and tests/source for exact behavior. Vendored calculator code, oracle inputs, embedded assets, and checksum manifests are provenance-sensitive and should be changed only through their owning workflow.
