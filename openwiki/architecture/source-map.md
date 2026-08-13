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
| Process instances | `src/instance/` | Candidate classification, focus, PID generations, tournament selection |
| Native access | `src/process/`, `src/platform/` | Linux/Windows process handles, memory reads, paths and OS behavior |
| Client decoding | `src/memory/`, `src/memory/stable/`, `src/memory/lazer/` | Offsets, signatures, pointers, snapshots, stable/lazer sentinels |
| State reduction | `src/state/` | Capture scheduling, stale-sample rejection, retention, gameplay state |
| Public APIs | `src/api/`, `src/model/api/` | v1, stream companion, v2, precise schemas and serialization |
| Server and transport | `src/server/`, `src/net/` | Exact routes, auth, files, pages, WebSockets, commands and streaming |
| Calculation | `src/calculation/`, `crates/vosu-pp/`, `crates/vosu-pp/rosu-map-api/`, `crates/vosu-pp/rosu-pp-api/` | Beatmaps, progressive state, live pp, legacy endpoint calculation, and provenance-pinned clean-room calculator APIs |
| Configuration and counters | `src/config/`, `src/counters/` | Canonical `vosu.toml`, one-time legacy dotenv migration, settings, discovery and downloads |
| Updating | `src/updater/` | Release selection, digest verification, staging, replacement, rollback |
| Reports and logging | `src/report/`, `src/logging/` | Report rendering, escaping, retention and operational diagnostics |
| Integration evidence | `tools/live-parity/`, `tools/oracle/` | Pinned parity captures, calculator provenance and diffs |
| Precise validation | `tools/input-validation/` | Map scanning, navigation, autoplay, edge accounting and capture artifacts |
| Behavioral contracts | `tests/`, `crates/vosu-test-harness/` | Compatibility, native, smoke, updater, state, performance and release tests |
| Performance evidence | `benches/hot_paths.rs`, [benchmark guide](../testing/benchmarks.md) | Criterion hot paths and machine-specific comparisons |
| Embedded inputs | `assets/`, `build.rs` | Dashboard, fonts, images, offsets and checksum-covered assets |

## Workspace boundaries

`Cargo.toml` defines one workspace with the production `vosu` package, the `vosu-pp` facade plus its `rosu-map-api` and `rosu-pp-api` members, and `crates/vosu-test-harness`. The latter is test support only. Release profiles use thin LTO, one codegen unit, stripped symbols, and abort-on-panic; `test-harness` is rejected by `build.rs` in release builds.

## Evidence hierarchy

Use `README.md` for user-facing behavior and prerequisites, the [architecture overview](overview.md) for current structural facts, [decision records](../decisions/index.md) for durable rationale, `AGENTS.md` for change verification and invariants, and tests/source for exact behavior. Vendored calculator code, oracle inputs, embedded assets, and checksum manifests are provenance-sensitive and should be changed only through their owning workflow.
