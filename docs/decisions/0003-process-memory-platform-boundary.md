---
type: Architecture Decision Record
title: Keep process-memory access behind platform boundary
description: Accepted decision to isolate native process handles, reads, discovery, and target architecture behind Linux and Windows backends.
tags: [architecture, decision, adr, process-memory, platform]
---

# ADR 0003: Keep process-memory access behind a platform boundary

## Status

Accepted

## Context

vosu reads another process on Windows and Linux, across 32-bit and 64-bit targets. Native handles, process discovery, readable-region enumeration, liveness checks, and batched syscalls differ by operating system. Managed-value decoding and stable/lazer state interpretation should not duplicate those differences.

## Decision

Define an object-safe `ProcessMemory` boundary for exact reads, batched reads, readable regions, process identity, and liveness. Let each operating-system backend own and release its native resource. Pair the backend with explicit target bitness in `ProcessHandle`; implement pointer arithmetic and managed-value decoding above the syscall boundary.

Keep client-layout knowledge above this layer: stable readers, lazer readers, signature scanning, and snapshot builders consume the same process handle rather than issuing platform syscalls directly.

The boundary lives in [`process.rs`](../../src/process.rs), with native implementations in [`process/windows.rs`](../../src/process/windows.rs) and [`process/linux.rs`](../../src/process/linux.rs). Higher-level consumers include [`memory/stable.rs`](../../src/memory/stable.rs), [`memory/lazer.rs`](../../src/memory/lazer.rs), and [`process/scanner.rs`](../../src/process/scanner.rs).

## Consequences

- OS-specific resource ownership and partial-read behavior are localized.
- Pointer width remains explicit even when the daemon and target use different bitness.
- Tests can supply in-memory backends without reproducing native process setup.
- Decoders retain one implementation and can batch reads without knowing the operating system.
- The object-safe boundary introduces dynamic dispatch at native read calls; collection decoding remains outside it so generic callbacks can stay monomorphized.
- Adding an operating system requires a complete backend plus discovery integration, not changes throughout every reader.

## Alternatives considered

- **Scatter conditional compilation through readers:** rejected because it would duplicate decoding and make safety properties depend on each call site.
- **Use a concrete platform enum in every read:** rejected because it would couple consumers to the current backend set and complicate test backends.
- **Treat pointers as host-sized without target metadata:** rejected because cross-bitness reads would decode addresses incorrectly.

## Related documentation

- [Architecture](../architecture/overview.md)
- [Benchmarks](../testing/benchmarks.md) — memory-read measurements are platform- and machine-specific observations.
