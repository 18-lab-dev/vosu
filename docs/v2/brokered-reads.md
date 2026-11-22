---
type: Benchmark Guide
title: Brokered memory-read cost (vosu 2.0 phase 0)
description: Measured cost of ADR 0019's `process`-runtime read broker versus direct reads, and the read-plan/transport recommendation it supports.
tags: [testing, benchmarks, v2, plugin-abi]
---

# Brokered memory-read cost

[ADR 0019](../decisions/0019-plugin-runtime-and-trust.md) brokers a
`process`-runtime plugin's game-memory reads through core: the plugin sends a
batched read plan, core executes it against the matched process and returns
the bytes. The ADR flags this round trip's cost as unmeasured. This document
measures it and recommends a read-plan message design and IPC transport for
`PluginAbiSpec` (`docs/v2/plugin-abi.md`) to adopt.

## Method

Benchmark: `benches/brokered_reads_bench.rs`. Helper target process:
`examples/brokered_reads_target.rs` (a synthetic process with two pointer-chain
buffers; no game client involved). Both are new code owned by this document;
see the bench file's module doc comment for the full plan-shape derivation.

### Plan shapes

Two read-plan shapes, derived from the 1.x readers:

- **Precise** (polled every gameplay tick): `src/memory/lazer/reader.rs`'s
  `key_overlay`/`global_precise` chase a deep, mostly-sequential pointer chain
  (game base → HUD overlay → input-count controller → trigger list → up to 18
  per-key trigger objects → count/flag fields), the same shape
  `benches/memory_reader_bench.rs`'s `Fixture::active_lazer` builds. Running
  that existing bench (`cargo bench --locked --bench memory_reader_bench`)
  measured, for one precise cycle (global + 18-key overlay + hit errors):
  `exact_reads_per_cycle=24`, `batch_reads_per_cycle=2`,
  `process_bytes_per_cycle=912` — 26 total native calls, most of them
  single-field pointer chases rather than batched reads. `precise_plan()`
  models the same shape structurally: one round trip per sequential pointer
  hop, plus two wide batched stages for the 18 per-key reads
  (`src/memory/lazer/global_batch.rs`'s `Stage<N>` groups independent reads the
  same way) — **8 stages, 43 reads, 358 bytes**.
- **Regular** (polled once per published snapshot): `src/state/readers.rs`'s
  `capture_regular` fans out to independent decoder calls (`global`, `menu`,
  `mp3_length`, `settings`, conditionally `gameplay`/`leaderboard`), each
  batching its own fields in one or two `Stage<N>` calls (seen in
  `src/memory/lazer/global_batch.rs` up to `Stage::<13>`). `regular_plan()`
  models one pointer-resolution stage for four independent decoder targets,
  then one batched field read per target — **5 stages, 8 reads, 116 bytes**.

The regular plan is structurally estimated (no existing instrumented bench
counts `capture_regular`'s calls directly), not measured like the precise
figures above; both synthetic shapes are a structural match to the real
decoders' round-trip and batching pattern, not a byte-for-byte replay.

### Variants

- **direct** — the bench process reads the synthetic target over
  `process_vm_readv` itself (`vosu::process::open_process`), as a 1.x /
  `in-process`-runtime plugin does. No IPC.
- **uds** — a forked, unprivileged "plugin host" sends the whole encoded plan
  to the parent ("core") over a `UnixStream` pair and gets back one response
  frame with every stage's bytes; core executes the dependent multi-stage
  chain locally, so this is **one round trip per sample** regardless of stage
  count.
- **shm** — identical protocol, transported over a `MAP_SHARED` region with
  two `eventfd`s for request/response wakeups instead of a socket.
- **naive** — the plugin host does the pointer chasing itself, issuing one
  request/response round trip per individual read instruction (43 round
  trips/sample for the precise plan) — the case a batched, pointer-chain-aware
  plan message exists to avoid.

Each variant runs 200 warm-up samples then 5,000 timed samples per plan shape.
CPU is `getrusage` user+system time: `RUSAGE_SELF` deltas around the serving
loop for core, `RUSAGE_CHILDREN` deltas (baseline snapshot before `fork`,
difference after `waitpid`) for the plugin host; both divided by sample count.

### Machine and commands

Ryzen 9 9950X3D (32 threads), kernel `7.3.5-2-cachyos-bore`, Rust 1.96.1.
CPUs 8-15,24-31 can be in use by a VM on this host, so the bench and every
process it forks or spawns were pinned off them:

```sh
cargo build --locked --release --example brokered_reads_target
taskset -c 2-5 cargo bench --locked --bench brokered_reads_bench
```

(`cargo bench` places the example and bench binary under `target/release/`;
the helper example must be built first since `cargo bench` does not build
examples. The bench locates it relative to its own executable path, not via
`CARGO_BIN_EXE_*`, because that variable only covers `[[bin]]` targets.)

## Results

Nanoseconds unless noted; p50/p99 are wall-clock round-trip latency per
sample, core/plugin CPU are per-sample `getrusage` time.

| Plan | Variant | Round trips/sample | p50 | p99 | Core CPU | Plugin CPU |
|---|---|---:|---:|---:|---:|---:|
| precise (43 reads, 358 B) | direct | 0 (in-process) | 7,434 | 8,166 | — | — |
| precise | uds | 1 | 16,071 | 21,721 | 12,436 | 4,041 |
| precise | shm+eventfd | 1 | 10,770 | 12,724 | 9,772 | 1,536 |
| precise | naive (per-read) | 43 | 266,783 | 325,344 | 155,244 | 122,914 |
| regular (8 reads, 116 B) | direct | 0 (in-process) | 3,486 | 4,118 | — | — |
| regular | uds | 1 | 12,423 | 17,433 | 7,864 | 3,806 |
| regular | shm+eventfd | 1 | 6,563 | 8,496 | 5,349 | 1,454 |
| regular | naive (per-read) | 8 | 49,614 | 63,860 | 28,967 | 23,049 |

Raw `METRIC key=value` output from the run above (`target/release/deps/brokered_reads_bench-*`),
repeated across three back-to-back runs to confirm stability (p50 varied by
less than 5% run to run, p99 by under 20% — expected scheduler noise for a
context-switch-bound measurement):

```
METRIC precise_direct_p50_ns=7434.0
METRIC precise_direct_p99_ns=8166.0
METRIC precise_direct_mean_ns=7462.9
METRIC precise_direct_core_cpu_ns=0.0
METRIC precise_direct_plugin_cpu_ns=0.0
METRIC precise_direct_round_trips=8
METRIC precise_direct_reads=43
METRIC precise_direct_bytes=358
METRIC precise_uds_p50_ns=16071.0
METRIC precise_uds_p99_ns=21721.0
METRIC precise_uds_mean_ns=15817.1
METRIC precise_uds_core_cpu_ns=12436.2
METRIC precise_uds_plugin_cpu_ns=4041.4
METRIC precise_uds_round_trips=1
METRIC precise_uds_reads=43
METRIC precise_uds_bytes=358
METRIC precise_shm_p50_ns=10770.0
METRIC precise_shm_p99_ns=12724.0
METRIC precise_shm_mean_ns=10854.5
METRIC precise_shm_core_cpu_ns=9772.2
METRIC precise_shm_plugin_cpu_ns=1536.4
METRIC precise_shm_round_trips=1
METRIC precise_shm_reads=43
METRIC precise_shm_bytes=358
METRIC precise_naive_p50_ns=266783.0
METRIC precise_naive_p99_ns=325344.0
METRIC precise_naive_mean_ns=268981.7
METRIC precise_naive_core_cpu_ns=155244.2
METRIC precise_naive_plugin_cpu_ns=122914.0
METRIC precise_naive_round_trips=43
METRIC precise_naive_reads=43
METRIC precise_naive_bytes=358
METRIC regular_direct_p50_ns=3486.0
METRIC regular_direct_p99_ns=4118.0
METRIC regular_direct_mean_ns=3510.4
METRIC regular_direct_core_cpu_ns=0.0
METRIC regular_direct_plugin_cpu_ns=0.0
METRIC regular_direct_round_trips=5
METRIC regular_direct_reads=8
METRIC regular_direct_bytes=116
METRIC regular_uds_p50_ns=12423.0
METRIC regular_uds_p99_ns=17433.0
METRIC regular_uds_mean_ns=11241.2
METRIC regular_uds_core_cpu_ns=7863.8
METRIC regular_uds_plugin_cpu_ns=3806.0
METRIC regular_uds_round_trips=1
METRIC regular_uds_reads=8
METRIC regular_uds_bytes=116
METRIC regular_shm_p50_ns=6563.0
METRIC regular_shm_p99_ns=8496.0
METRIC regular_shm_mean_ns=6436.1
METRIC regular_shm_core_cpu_ns=5349.0
METRIC regular_shm_plugin_cpu_ns=1454.4
METRIC regular_shm_round_trips=1
METRIC regular_shm_reads=8
METRIC regular_shm_bytes=116
METRIC regular_naive_p50_ns=49614.0
METRIC regular_naive_p99_ns=63860.0
METRIC regular_naive_mean_ns=50275.4
METRIC regular_naive_core_cpu_ns=28966.8
METRIC regular_naive_plugin_cpu_ns=23049.0
METRIC regular_naive_round_trips=8
METRIC regular_naive_reads=8
METRIC regular_naive_bytes=116
```

## Findings

- **Brokering a batched plan costs about 1–2 context-switch round trips, not
  one per read.** uds adds roughly 8.6 µs (precise) / 9.0 µs (regular) of p50
  latency over direct reads; shm+eventfd adds roughly 3.3 µs / 3.1 µs. Both
  are flat with respect to read count once the plan is batched: precise (43
  reads) and regular (8 reads) see nearly the same per-sample transport
  overhead, because both cost exactly one round trip.
- **Per-read RPC (naive) is 16–25x slower than a batched plan**, and scales
  with op count, not just round trips: precise (43 ops) costs about 18x its
  uds p50 and regular (8 ops) about 4x its uds p50. A `process`-runtime plugin
  ABI that lets the plugin issue one remote call per field (rather than one
  read plan with core-side pointer dereferencing) would make the precise path
  — the one polled every gameplay tick — unusable.
- **shm+eventfd beats a Unix domain socket** by roughly 1.4–1.9x at p50 and
  1.7–2x at p99 for both plan shapes, with lower plugin-side CPU (eventfd wake
  + shared-memory read skips a socket's copy and the kernel's connection
  bookkeeping). It also reports visibly tighter p99 tails in this run (12.7 µs
  vs 21.7 µs for precise), though three runs is not enough to bound tail
  variance precisely.
- **Core-side CPU dominates plugin-side CPU** for both brokered transports
  (uds: ~12.4 µs vs ~4.0 µs for precise; shm: ~9.8 µs vs ~1.5 µs), because core
  does the actual `process_vm_readv` work in both designs — brokering moves
  *where* memory is read from, not *how much* work reading it costs.
- The precise plan's direct-read p50 (7.4 µs) already exceeds the regular
  plan's brokered-over-shm p50 (6.6 µs): a deep, 43-read pointer chain read
  in-process can cost more raw syscall time than a shallow, 8-read plan
  brokered over shared memory. Round-trip count and pointer-chain depth matter
  more than the raw number of bytes transferred for either in-process or
  brokered reads.

## Recommendation

For `PluginAbiSpec`'s `process`-runtime read path:

1. **The read-plan message must carry the full dependent pointer chain in one
   request**, exactly as this bench's `uds`/`shm` variants do: a sequence of
   stages, each a batch of independent reads (base = plan root or a prior
   stage's resolved pointer, plus a byte offset and length), with core
   resolving every dereference and returning all stage bytes in one response.
   A plan that can express only flat, independent reads (forcing the plugin to
   resolve pointers itself and issue follow-up reads) reproduces the naive
   variant's 16–25x cost and must not ship.
2. **Transport: prefer shared memory with `eventfd` wakeups over a Unix domain
   socket** for the Linux `process`-runtime transport, given its consistent
   1.4–2x latency and lower CPU advantage in this benchmark at this message
   size (plans around 1 KiB, responses around 1 KiB). A `UnixStream` remains
   an acceptable fallback (e.g. if shared-memory setup/teardown per plugin
   proves more complex to make robust against a crashed plugin host than a
   socket close) since both satisfy the hard requirement — one round trip
   regardless of op count.
3. **Size the plan generously**: 64 KiB covers every plan and response
   measured here with wide margin; `PluginAbiSpec` should set an explicit
   upper bound (e.g. reject a plan whose encoded size or declared read count
   exceeds a fixed limit) so a malformed or adversarial plugin cannot make
   core allocate unbounded buffers per request.

## Open questions

- **Windows transport is unmeasured.** ADR 0019 names named pipes and shared
  sections as the Windows analogue of this benchmark's uds/shm variants.
  Measuring it needs a Windows-native equivalent of this bench (`CreateNamedPipe`
  or `CreateFileMapping`/`MapViewOfFile` plus a Windows event object instead of
  `eventfd`) built and run on the native Windows guest, which this Linux-host
  assignment could not do. Recommendation: before freezing the Windows side of
  `PluginAbiSpec`'s transport, port `benches/brokered_reads_bench.rs`'s
  protocol (same plan encoding, same two plan shapes) to a Windows helper
  binary and compare named pipes against a shared section + event object on
  the native Windows guest, pinned the same way this bench pins Linux CPUs.
- **Only two plan shapes were measured.** `capture_regular`'s round-trip count
  is a structural estimate (see Method), not instrumented; if the real
  `process`-runtime read path ends up issuing fields across many more
  independent decoder stages than modeled here (e.g. uncoalesced tourney or
  leaderboard reads), its round-trip count could exceed this benchmark's
  5-stage regular estimate. Recommendation: once a game plugin exists on the
  `process` runtime, extend `memory_reader_bench`'s `CountingMemory` pattern
  to the regular capture path and confirm the real stage count stays close to
  this estimate.
