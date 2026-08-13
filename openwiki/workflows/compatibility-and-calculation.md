---
type: Compatibility Workflow
title: vosu compatibility and calculation workflows
description: Guide to preserving vosu’s HTTP/WebSocket schemas, precise state, selected-field subscriptions, live calculation, legacy calculation, and parity evidence.
resource: src/api/snapshot.rs
tags: [compatibility, api, websocket, calculation, parity]
---

# Compatibility and calculation workflows

## Public projections

The API surface intentionally contains four schemas: gosuMemory-compatible v1, streamCompanion-compatible state, vosu v2, and v2 precise. All are built from the atomic `CombinedSnapshot` owned by `SnapshotManager`; do not add route-level reads of regular, precise, focus, or lifecycle state.

WebSocket hubs keep compatibility connections separate from selected-field v2 subscriptions. Recent commit `9b67c55` added the selected-field endpoint; `1f28910` hardened close handshakes; `cb676ee` added precise keypress state and timestamps. Each hub bounds pending state output and keeps control replies separate from state-frame accounting.

The current compatibility models expose `hitErrors`/`hitErrorArray` as an object with `all`, `press`, and `release` arrays rather than a flat JSON array. The reducer preserves the aggregate stream while classifying press/release errors; lazer mania hold aggregates can reclassify a prior press through `release_correction`. This shape is propagated through v1, stream companion, v2, precise, play, and tournament projections, so treat it as a wire-contract change and update consumers and fixtures together (`src/model/states.rs`, `src/state/bundle/reducer.rs`, `src/model/api/`, `tests/state_api_compat.rs`).

## Precise state

Precise polling has its own cadence and latest-only capture slot. The additive precise keypress object reports active mode, ordered logical keys, press state/counts, and nullable edge timestamps. Timestamps are only updated when sampled state exposes the edge; a count jump must not fabricate timestamps. Changes should be checked across memory readers, models, reducer state, API builders, WebSocket projection, tests, and the `tools/input-validation` harness.

## Calculation contracts

Live calculation is state-driven: beatmap readiness, parsing/cache state, gameplay progression, and result updates flow through `src/calculation/` and the clean-room `crates/vosu-pp` facade (with `rosu-map-api` and `rosu-pp-api` workspace members). Results are accepted only when state keys and calculation generations still match. The HTTP calculation endpoint is a distinct legacy rosu-pp-compatible builder that accepts query parameters and must preserve its setter/order semantics. Sharing the live contract with the endpoint would change compatibility behavior.

V2 beatmap stats now expose `timingWindows` when a map is parsed. The values come from the calculation engine’s mode-aware hit windows after mods: temporal windows are clock-rate-scaled, while osu!catch’s `300` is its spatial half-width. An unparsed/missing map serializes an empty object. Do not expose `stars.hitWindow` for osu!catch: its `timingWindows.300` is spatial, not a temporal great window; `stars.hitWindow` applies only when the parsed engine provides a temporal great window. Verify mode/mod combinations rather than treating these as fixed difficulty metadata (`src/api/v2/builders.rs`, `src/api/v2/tests.rs`, `src/model/api/v2.rs`, `crates/vosu-pp/src/scoring.rs`).

JSON number formatting is shared across v1, stream companion, v2, and runtime projection writers through `src/calculation/root/helpers.rs::write_js_json_number`. The formatter writes finite ECMAScript-compatible numbers directly to the output, emits `null` for non-finite values, normalizes signed zero to `0`, and avoids the former temporary `String` allocation; boundary tests in `src/calculation/root/tests.rs` cover the representation thresholds and special values. Preserve this helper when changing serializers rather than reintroducing per-value allocation.

Pinned calculator, map-parser, and osu! framework revisions are declared in `Cargo.toml` metadata and evidenced under `tools/oracle/`, `crates/vosu-pp/rosu-map-api/`, and `crates/vosu-pp/rosu-pp-api/`. Treat these clean-room API forks and their licenses/source mappings as provenance rather than ordinary application code.

## Verification workflow

Use targeted compatibility tests first, then live parity when observable state or wire timing changes. `tools/live-parity/run.py` validates strict configuration, attested processes/sockets, pinned artifacts, and the menu → active play → paused → result → close lifecycle. It compares vosu against the pinned upstream reference for the declared Windows stable/lazer and Linux lazer matrix.

The [testing and verification guide](../testing/verification.md) is the verification owner for commands and evidence interpretation. The [runtime architecture](../architecture/overview.md) explains why atomic snapshots and generation checks are non-negotiable.
