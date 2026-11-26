---
type: Architecture Decision Record
title: Keep calculation contracts and engines separate
description: Accepted decision to preserve distinct live-state and legacy endpoint calculation contracts and compatibility semantics.
tags: [architecture, decision, adr, calculation, compatibility]
---

# ADR 0004: Keep calculation contracts and engines separate

## Status

Accepted

## Context

vosu exposes compatible calculations while stable and lazer scoring do not share one interchangeable engine contract. Beatmap parsing, mod conversion, gradual gameplay calculation, result calculation, and asynchronous application to state also change for different reasons. Treating them as one utility would make compatibility-sensitive call order and scoring selection implicit.

## Decision

Keep compatibility helpers separate from performance engines. Preserve the legacy `rosu-pp` builder contract in `LegacyRosuEngine`, and expose the live lazer calculator through the `vosu-pp` Rust facade and its provenance-pinned API members; do not route both through one nominal backend.

Adapt engines to typed beatmap/progressive requests, then place asynchronous state integration behind `StateCalculationBackend`. Key each result with instance generation, reset epoch, beatmap checksum, mode, and mods so only a matching state accepts it. Pin calculator provenance in Cargo package metadata rather than implying that any dependency update is compatible.

The separation is visible in [`vosu-osu-calc/src/lib.rs`](../../crates/vosu-osu-calc/src/lib.rs), [`vosu-osu-calc/src/engine.rs`](../../crates/vosu-osu-calc/src/engine.rs), [`state/state_backend.rs`](../../crates/vosu-osu-state/src/state_backend.rs), [`state/bundle/calculation.rs`](../../crates/vosu-osu-state/src/bundle/calculation.rs), [`crates/vosu-pp`](../../crates/vosu-pp), and [`Cargo.toml`](../../Cargo.toml).

## Consequences

- Legacy and lazer scoring behavior can evolve without silently substituting one contract for the other.
- Engine-specific parsing and conversion stay behind typed adapters while state reduction depends on one asynchronous port.
- Provenance and exact dependency versions are reviewable inputs to compatibility.
- Some models and mappings are necessarily duplicated at adapter boundaries.
- A calculation result may be discarded after doing work if its keyed state has already changed; correctness takes precedence over applying stale output.

## Alternatives considered

- **Use one calculator for both client contracts:** rejected because the code preserves distinct legacy builder semantics and live lazer semantics.
- **Call calculation crates directly from state reduction:** rejected because engine details, I/O, caching, and stale-result validation would become entangled with polling state.
- **Accept any completed asynchronous result:** rejected because map, mods, client generation, or reset epoch may have changed while it ran.

## Related documentation

- [Architecture](../architecture/overview.md)
- [Benchmarks](../testing/benchmarks.md) — calculator results and timings are tied to the recorded code, inputs, build, and machine.

## M4a path-authority amendment (2026-11-24)

A4.4 confines legacy calculation `path` to the captured current session's
declared roots: a relative child of its songs root, or an absolute spelling
canonicalizing within one of its declared roots. The eventual open uses
retained-root confinement, not validation followed by a pathname reopen.
Outside-root requests are the named `legacy-calculate-declared-roots`
security divergence; query/mod/result semantics and `LegacyRosuEngine` stay
unchanged. No-path calculation reads the captured published chart through
the [surface resource seam](../v2/adapter-surfaces.md#r0-resource-bytes-and-lifetime-budgets).

## M4b legacy HTTP amendment (2026-11-26)

`GET /api/calculate/pp` uses the shared adapter JSON media type
(`application/json; charset=utf-8`). It writes successful result numbers with
ECMAScript `JSON.stringify` spelling without rounding the result and preserves
the legacy engine and numeric bitmasks.

The adapter deliberately accepts well-formed acronym pairs (e.g. `HDDT`)
and valid structured JSON mod inputs including settings. The pinned
upstream's HTTP bridge coerces those strings to `NaN` and returns 500.
The `legacy-calculate-mod-inputs` contract row covers exactly the
`calculation.mods-acronyms` and `calculation.mods-json` golden cases.

Malformed accuracy, out-of-range clock rate (outside `0.01..=100`),
malformed acronym, invalid unsigned count, and invalid ruleset (outside
`0..=3`) return 400 with a JSON `{"error": string}` response. The pinned
upstream may calculate from its coerced argument or emit a WASM-decoder
500. The `legacy-calculate-client-errors` contract row covers exactly
`calculation.invalid-accuracy`, `calculation.invalid-clock-rate`,
`calculation.mods-invalid`, `calculation.invalid-count`, and
`calculation.invalid-mode`. Valid inputs keep their existing calculation
semantics.

The [divergence ledger](../../tests/fixtures/upstream/DIVERGENCES.md) and
[golden contract §10](../../tests/fixtures/upstream/GOLDEN_CONTRACT.md#10-divergence-subtraction-a4b11)
govern replay. Keep captured upstream responses intact and scope any
alternate vosu expectation to the listed cases and affected record parts.
