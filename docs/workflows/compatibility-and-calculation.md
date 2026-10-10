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

The compatibility models preserve the flat `hitErrors`/`hitErrorArray` arrays and add sibling `pressHitErrors` and `releaseHitErrors` arrays. Internally the reducer retains aggregate, press, and release streams; lazer mania hold aggregates can reclassify a prior press through `release_correction`. Keep these classifications consistent across v1, stream companion, v2, precise, play, and tournament projections without replacing the original wire arrays (`src/model/states.rs`, `src/state/bundle/reducer.rs`, `src/model/api/`, `tests/state_api_compat.rs`).

## Precise state

Precise polling has its own cadence and latest-only capture slot. The additive precise keypress object reports active mode, ordered logical keys, press state/counts, and nullable edge timestamps. Timestamps are only updated when sampled state exposes the edge; a count jump must not fabricate timestamps. Changes should be checked across memory readers, models, reducer state, API builders, WebSocket projection, tests, and the `tools/input-validation` harness.

On Windows, each active instance's worker clock holds a balanced 1 ms multimedia
timer-resolution request for its lifetime. No request is held solely because the
daemon is running without clients. This keeps the precise worker's timed waits
from inheriting the coarse default Windows timer quantum.

Stable v2 `session.playTime` is the client session clock, not beatmap playback
time. The memory reader follows the clock instruction's address operand, and the
regular reducer publishes that clock even when a newer precise sample owns the
playback time. Updating the session clock must not roll back precise state.

## Calculation contracts

Live calculation is state-driven: beatmap readiness, parsing/cache state, gameplay progression, and result updates flow through `src/calculation/` and the `crates/vosu-pp` Rust facade (with `rosu-map-api` and `rosu-pp-api` workspace members). Results are accepted only when state keys and calculation generations still match. The HTTP calculation endpoint is a distinct legacy rosu-pp-compatible builder that accepts query parameters and must preserve its setter/order semantics. Sharing the live contract with the endpoint would change compatibility behavior.

V2 beatmap stats expose `hitWindow` when a map is parsed. It contains the optional numeric judgement fields `miss`, `meh`, `ok`, `good`, `great`, and `perfect`; unsupported judgements are omitted. Values are maximum absolute timing errors in milliseconds after the active clock rate is applied. An unparsed/missing map serializes an empty object. Catch publishes all six judgements as zero, matching upstream's `EmptyHitWindows`, while `stars.hitWindow` is absent. Do not describe catch's spatial catcher width as a timing window (`src/api/v2/builders.rs`, `crates/vosu-pp/src/hit_windows.rs`, `src/model/api/v2.rs`).

Beatmap maximum combo comes from difficulty attributes across v1, v2, and
StreamCompanion. In mania it includes hold-duration ticks; it must not be
replaced by the maximum score's hit-result count.

JSON number formatting is shared across v1, stream companion, v2, and runtime projection writers through `src/calculation/root/helpers.rs::write_js_json_number`. The formatter writes finite ECMAScript-compatible numbers directly to the output, emits `null` for non-finite values, normalizes signed zero to `0`, and avoids the former temporary `String` allocation; boundary tests in `src/calculation/root/tests.rs` cover the representation thresholds and special values. Preserve this helper when changing serializers rather than reintroducing per-value allocation.

Pinned calculator, map-parser, and osu! framework revisions are declared in `Cargo.toml` metadata and evidenced under `tools/oracle/`, `crates/vosu-pp/rosu-map-api/`, and `crates/vosu-pp/rosu-pp-api/`. Treat these pinned API forks and their licenses/source mappings as provenance rather than ordinary application code.

## Upstream tosu 4.25.0 → 2359305 coverage

vosu tracks the native Rust-relevant behavior from this upstream range, not the
Electron application wholesale. Implemented coverage includes the max-achievable
and full-combo score/accuracy helpers; per-object nested tick-score metadata;
graph bounds based on visible objects; mode-aware v2 `hitWindow` timing fields
with clock rate applied to temporal windows (Catch reports zeros); and
native multiplayer room reading/reduction/API projection, including optional
`avatarUrl`. StreamCompanion output includes normalized token arrays and
ScoreV2 state/simulation support. Stable falsey settings are preserved, and
stable/relax score simulation and UR use the corrected sum-of-squares method
with dynamic clock rate. These describe implemented Rust behavior, not a claim
of complete behavioral or live parity with upstream.

The range also contains Electron packaging, updater/UI/overlay, CI and
repository-maintenance changes that do not apply to vosu's daemon contract.
vosu has no daemon Node/.NET dependency and does not port the Electron overlay.
The offset generator is a separate explicit maintainer/user tool; its bundled
runtime is not a daemon dependency.

Live compatibility still requires the native live-reader checks against the
supported client/runtime matrix. No live interactive parity result is asserted
here; offsets provenance or fixture/calculator evidence alone is not live
verification. The current calculator oracle fixture is generated against
`osu-pp-calculator` 0.6.1 (`323bc04198b256b5a00f4e63c5c4cbb506f43cc5`),
as pinned in `tools/oracle/oracle-lock.json`. The calculator diff manifest retains
baseline source pins and maintained replay coverage. Its obsolete untyped
`convert(99)` assertion was removed: Rust conversion accepts a typed `Ruleset`,
not the JavaScript wrapper's invalid-number fallback. `reference/lazer-calculator`,
historical osu inputs, and external C# test evidence remain baseline artifacts,
not evidence of current native execution.

## Verification workflow

Use targeted compatibility tests to gate changes to observable state or wire timing. Live parity is an optional diagnostic for suspected divergence from upstream ([ADR 0011](../decisions/0011-live-parity-optional.md)). `tools/live-parity/run.py` validates strict configuration, attested processes/sockets, pinned artifacts, and the menu → active play → paused → result → close lifecycle. It compares vosu against the pinned upstream reference for the declared Windows stable/lazer and Linux lazer matrix.

Active v1/v2 observations are matched in receipt order within the same state
and confirmed retry segment. Matching first maximizes one-to-one reference
coverage, then minimizes total beatmap-clock skew, then receipt-time skew.
This prevents extra vosu frames from biasing pairs toward the oldest eligible
frame without consuming observations needed by later reference frames.
The fixed 300 ms window, payload comparators, and unmatched-reference failures
remain unchanged; neither clock agreement nor a denser stream waives coverage.

The [testing and verification guide](../testing/verification.md) is the verification owner for commands and evidence interpretation. The [runtime architecture](../architecture/overview.md) explains why atomic snapshots and generation checks are non-negotiable.
