---
type: Architecture Decision Record
title: One daemon owns deterministic multi-instance lifecycle
description: Accepted decision for one daemon to own deterministic discovery, generations, selection, and publication across multiple osu! processes.
tags: [architecture, decision, adr, lifecycle, multi-instance]
---

# ADR 0001: One daemon owns a deterministic multi-instance lifecycle

## Status

Accepted. For vosu 2.0, [ADR 0020](0020-game-agnostic-data-model.md) extends it from osu! processes to sessions of any game plugin; deterministic generations and selection remain.

## Context

A running vosu daemon can observe stable and lazer clients, tournament processes, and more than one process of the same client type. Public endpoints still need a stable selected source, while tournament views need the complete roster. Process enumeration order and foreground-window availability are not reliable identity or ordering contracts.

## Decision

Keep all validated game processes in one daemon-owned lifecycle. Assign a monotonically increasing generation when a newly discovered PID is accepted, and retain instances in PID order. Opening a candidate must yield a live handle whose reported PID agrees with discovery.

Selection is deterministic: a matching tournament-manager instance wins; otherwise the lowest matching PID wins. Focus is separate metadata rather than the sole ownership mechanism. Publish the ordered roster, focus, readiness, and offset readiness as a semantic lifecycle snapshot; do not create an independent API server per game process.

Implementation now lives in core's [`DiscoveryService`](../../crates/vosu-core/src/plugin/discovery.rs), osu! classification and session lifecycle in [`vosu-osu`](../../crates/vosu-osu/src/plugin.rs) and [`session.rs`](../../crates/vosu-osu/src/session.rs), selection in core’s Focus rule, lifecycle composition in [`runtime/lifecycle.rs`](../../crates/vosu-core/src/runtime/lifecycle.rs), and publication in the [state bus](../../crates/vosu-core/src/state.rs).

The former `src/instance` implementation was removed in M1.

## Consequences

- One listener can preserve the existing API surface while representing several clients.
- PID ordering and generations make selection and stale-work rejection reproducible.
- Client-specific setup, including lazer offset resolution, belongs to an instance generation and is cancelled with it.
- The daemon must reconcile discovery, process death, focus, tournament roles, and readiness before publication.
- Foreground focus may affect reported focus without changing which processes the daemon owns.

## Alternatives considered

- **Use process enumeration order:** rejected because the provider does not define a stable order.
- **Track only the foreground process:** rejected because focus can be unavailable and tournament/all-instance output needs the full roster.
- **Run one daemon per game process:** rejected because it would introduce listener ownership and API aggregation problems absent from the current design.

## Related documentation

- [Architecture](../architecture/overview.md)
- [Benchmarks](../testing/benchmarks.md) — discovery and publication measurements, when present, are observations rather than selection guarantees.
