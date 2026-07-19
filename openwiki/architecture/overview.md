---
type: Architecture Guide
title: vosu runtime architecture overview
description: Source-grounded overview of vosu’s single-daemon composition root, process discovery, native memory boundary, regular and precise sampling, atomic snapshots, calculations, and HTTP/WebSocket publication.
resource: docs/architecture.md
tags: [architecture, runtime, state, api, concurrency]
---

# Runtime architecture overview

`src/main.rs` is intentionally thin: it constructs Tokio, calls `runtime::run_cli`, maps the active-owner conflict to exit status 75, and bounds shutdown. `src/runtime/lifecycle.rs::ProductionRuntime` is the composition root for configuration, logging, listener, WebSocket hubs, counters, instance runtime, state supervisor, updater, and shutdown ownership.

## End-to-end flow

```text
osu! process
  -> NativeProcessProvider / InstanceManager
  -> ProcessHandle and stable/lazer MemoryReader
  -> regular + precise state workers
  -> serialized StateBundle reducer
  -> per-generation publication
  -> SnapshotManager / CombinedSnapshot
  -> v1, stream companion, v2, precise HTTP and WebSocket schemas
```

[Runtime and configuration workflows](../workflows/runtime-and-config.md) control startup, reload, listener replacement, and shutdown around this flow. [Compatibility and calculation workflows](../workflows/compatibility-and-calculation.md) explain the public projections and calculation contracts.

## Discovery and memory

`src/instance/` classifies stable and lazer candidates, assigns monotonically increasing generations, handles focus and tournament selection, and treats PID reuse as a new process. `src/process.rs` defines the object-safe native boundary; Linux and Windows backends own handles and platform calls. Reads are all-or-error, address arithmetic and lengths are checked, and decoding stays above the syscall layer.

`src/memory/stable/*` and `src/memory/lazer/*` implement the common `MemoryReader` surface while preserving client-specific layouts and sentinels. `ReadStatus` distinguishes ready, not-ready, unchanged, unsupported, and failure states. Regular and precise reader instances are independent because blocking capture threads cannot share mutable pointer caches; only validated synchronization state is transferred.

## Sampling, reduction, and generations

Each ready process generation has regular and precise blocking capture paths plus one serialized reducer (`src/state/worker.rs`). Regular capture uses one-slot backpressure; precise capture is latest-only so high-frequency observations overwrite stale work instead of accumulating. The reducer rejects stale source/generation/sequence/context samples, applies retention and gameplay transitions in `src/state/bundle/`, and emits immutable state.

`ProductionStateSupervisor` accepts worker publications only when PID and generation are still desired. This prevents stale workers, reused PIDs, and late calculations from replacing current state. The unbounded worker-to-supervisor publication channel remains a documented memory-pressure risk under a persistently stalled supervisor.

## Atomic publication and server boundary

`SnapshotManager` publishes one immutable combined snapshot containing lifecycle metadata, focus, all instances, regular state, precise state, and runtime settings. API builders consume this snapshot rather than independently loading state pieces, preventing impossible mixed-time payloads. HTTP routing in `src/server/routes.rs` classifies exact method/path combinations, applies authorization and body limits, then delegates through provider traits; routes never read game memory directly.

The server has separate compatibility hubs for v1, stream companion, v2, precise, and selected-field v2 subscriptions. Each client has bounded pending state output. Shutdown seals registration, sends WebSocket close 1001, drains callbacks, and only then releases downstream providers. Listener replacement retains the old effective listener if resolution or bind fails.

## Calculations

Live calculation is state-driven and generation-aware: beatmap parsing, progressive gameplay, and `vosu-lazer-pp` work feed results back only when their state key and calculation generation are current. The calculation endpoint is a separate legacy rosu-pp-compatible contract in `src/calculation/engine.rs`; it must not be silently unified with live calculation.

## Change hotspots

Preserve atomic snapshots, generation checks, bounded precise/WebSocket paths, all-or-error native reads, platform separation, and calculation contract boundaries. Structural changes belong with [decision records](../../docs/decisions/README.md); measured hot-path changes belong in [benchmarks](../../docs/benchmarks.md). See the [source map](source-map.md) for ownership by directory.
