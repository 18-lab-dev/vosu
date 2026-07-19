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

## Precise state

Precise polling has its own cadence and latest-only capture slot. The additive precise keypress object reports active mode, ordered logical keys, press state/counts, and nullable edge timestamps. Timestamps are only updated when sampled state exposes the edge; a count jump must not fabricate timestamps. Changes should be checked across memory readers, models, reducer state, API builders, WebSocket projection, tests, and the `tools/input-validation` harness.

## Calculation contracts

Live calculation is state-driven: beatmap readiness, parsing/cache state, gameplay progression, and result updates flow through `src/calculation/` and `crates/vosu-lazer-pp`. Results are accepted only when state keys and calculation generations still match. The HTTP calculation endpoint is a distinct legacy rosu-pp-compatible builder that accepts query parameters and must preserve its setter/order semantics. Sharing the live contract with the endpoint would change compatibility behavior.

Pinned calculator and upstream revisions are declared in `Cargo.toml` metadata and evidenced under `tools/oracle/` and the vendored `vosu-lazer-pp` sources. Treat these inputs as provenance rather than ordinary application code.

## Verification workflow

Use targeted compatibility tests first, then live parity when observable state or wire timing changes. `tools/live-parity/run.py` validates strict configuration, attested processes/sockets, pinned artifacts, and the menu → active play → paused → result → close lifecycle. It compares vosu against the pinned upstream reference for the declared Windows stable/lazer and Linux lazer matrix.

The [testing and verification guide](../testing/verification.md) is the verification owner for commands and evidence interpretation. The [runtime architecture](../architecture/overview.md) explains why atomic snapshots and generation checks are non-negotiable.
