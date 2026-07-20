---
type: Architecture Decision Record
title: Publish immutable, latest-only API snapshots
description: Accepted decision to reduce state into immutable atomic snapshots and coalesce pending publications to the latest state per process.
tags: [architecture, decision, adr, snapshots, concurrency]
---

# ADR 0002: Publish immutable, latest-only API snapshots

## Status

Accepted

## Context

Regular memory reads, precise reads, lifecycle changes, calculation results, and API serialization proceed at different rates. An API response must not combine focus metadata from one instant with state from another. Readers are latency-sensitive and do not need every intermediate polling result.

## Decision

Reduce mutable worker state into immutable, `Arc`-shared snapshots. Pair manager metadata and the selected instance state in one `CombinedSnapshot`, then atomically replace the current publication through `ArcSwap`. Serialize publishers, keep readers lock-free, and retain no publication queue or history.

Use process generations and reset epochs to identify the state lineage, and calculation keys to reject asynchronous results for an obsolete lineage or beatmap. Share regular and precise components independently so unchanged data and schema encodings can be reused by pointer identity and semantic revision.

The contracts are implemented in [`api/snapshot.rs`](../../src/api/snapshot.rs), [`state/bundle.rs`](../../src/state/bundle.rs), [`state/bundle/calculation.rs`](../../src/state/bundle/calculation.rs), and [`state/worker.rs`](../../src/state/worker.rs).

## Consequences

- Each request observes one self-consistent manager/focus/state publication.
- Slow readers may skip intermediate states; the latest state is the contract.
- Readers avoid a shared mutable-state lock, while publication allocates immutable state only when components change.
- Unfiltered schema encodings can be computed once and inherited when all declared dependencies are unchanged.
- Generation, epoch, and calculation-key checks add explicit bookkeeping but prevent late work from corrupting a newer state.
- This is not an event log and cannot answer historical-state queries.

## Alternatives considered

- **Expose a mutex-protected mutable model:** rejected because readers could contend with polling and serialization would need careful lock scoping.
- **Publish manager, regular, and precise values independently:** rejected because readers could assemble an impossible cross-generation state.
- **Queue every update for every consumer:** rejected because API consumers require current state, not guaranteed replay, and backpressure would couple them to polling.

## Related documentation

- [Architecture](../architecture/overview.md)
- [Benchmarks](../testing/benchmarks.md) — snapshot and encoding results characterize measured builds and machines, not a latency promise.
