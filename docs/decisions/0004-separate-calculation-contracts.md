# ADR 0004: Keep calculation contracts and engines separate

## Status

Accepted

## Context

vosu exposes compatible calculations while stable and lazer scoring do not share one interchangeable engine contract. Beatmap parsing, mod conversion, gradual gameplay calculation, result calculation, and asynchronous application to state also change for different reasons. Treating them as one utility would make compatibility-sensitive call order and scoring selection implicit.

## Decision

Keep compatibility helpers separate from performance engines. Preserve the legacy `rosu-pp` builder contract in `LegacyRosuEngine`, and expose the live lazer calculator through the dedicated workspace crate `vosu-lazer-pp`; do not route both through one nominal backend.

Adapt engines to typed beatmap/progressive requests, then place asynchronous state integration behind `StateCalculationBackend`. Key each result with instance generation, reset epoch, beatmap checksum, mode, and mods so only a matching state accepts it. Pin calculator provenance in Cargo package metadata rather than implying that any dependency update is compatible.

The separation is visible in [`calculation.rs`](../../src/calculation.rs), [`calculation/engine.rs`](../../src/calculation/engine.rs), [`calculation/state_backend.rs`](../../src/calculation/state_backend.rs), [`state/bundle/calculation.rs`](../../src/state/bundle/calculation.rs), [`crates/vosu-lazer-pp`](../../crates/vosu-lazer-pp), and [`Cargo.toml`](../../Cargo.toml).

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

- [Architecture](../architecture.md)
- [Benchmarks](../benchmarks.md) — calculator results and timings are tied to the recorded code, inputs, build, and machine.
