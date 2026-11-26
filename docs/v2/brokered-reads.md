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

## Windows transport candidates (M5 P1, candidate result)

**Status: candidate result for Main to approve under A5.6. It is not D17
evidence.** The runs below share their machines with other agents, so they
are exploratory: the load each run saw is recorded with it. The production
arm (`production_sync` and `production_async`, which need B0 and E0) is not
part of this experiment. The sections above stay the historical prototype
record.

### Method

`benches/brokered_reads_bench.rs` now runs on Linux and Windows. Its module
doc lists the variants, the metrics and the exact arguments it accepts. The
plan shapes are frozen and asserted at start: precise is 8 stages, 43 reads
and 358 bytes, and regular is 5 stages, 8 reads and 116 bytes.

- **Same plans and bytes.** Every variant submits the same encoded plans,
  which `benches/support/brokered/plans.rs` defines once. Core serves each
  one with the same executor against real target memory, using
  `process_vm_readv` on Linux and `ReadProcessMemory` on Windows. The target
  (`examples/brokered_reads_target.rs`) walks the same plans over its own
  memory, with no remote read or transport, and prints the SHA-256 of the
  exact response. A variant whose `result_sha256` differs from that hash or
  from `direct` fails the run.
- **Candidates.** Core creates the endpoint and spawns a dedicated
  plugin-side helper, which is the bench executable re-run with
  `--p1-child`. The helper inherits only its endpoint and drives and times
  the round trips. It is not the plugin supervisor.
  - `section` (Windows) and `ring` (Linux, the reference) are T0's
    production ring: `vosu_core::plugin::process::channel` over
    `RingObjects`, with one unnamed section and auto-reset event per
    direction on Windows, and a sealed memfd and an eventfd on Linux.
  - `pipe` (Windows, measurement only) is a byte-mode named pipe
    (`benches/support/brokered/pipe_windows.rs`).
- **Identical batching, flushing and checks.** Each sample is one
  `READ_PLAN_SUBMIT` frame with `VOSU_READ_PLAN_SYNC` and one
  `READ_PLAN_RESULT` frame, so there is one logical submission and two wire
  frames. Both carriers use the T0 frame head and 1 MiB cap, apply the frozen
  `DirectionPolicy` to each frame before copying its payload, copy into a
  reusable 8-byte-aligned buffer, publish each frame with one write, never
  flush, and wait only by blocking.
- **Matched security.** Both carriers reach the helper only by inheritance
  of exactly their handles: four for the ring, the one client end for the
  pipe. The bench marks them inheritable around a single `Command::spawn`;
  production spawn code (L0/W0) uses an explicit allowlist instead. The pipe
  gets an unguessable 128-bit name, a DACL that grants only the current
  user, `FILE_FLAG_FIRST_PIPE_INSTANCE`, a single instance and
  `PIPE_REJECT_REMOTE_CLIENTS`. Core opens its only client end without
  impersonation rights before the helper exists. The one difference left is
  that the name stays listed under `\\.\pipe\` while the pipe exists, but
  nothing else can connect to it. On Linux each helper checks that its open
  descriptors are exactly stdio plus its endpoint.
- **Metrics.** Each side measures itself. The helper's figures cover only
  measured loops: the timed samples and Criterion's batches. Core's cover the
  whole session from `READY` to `DESTROYED`, divided by the plans it served.
  - `p50_ns` and `p99_ns` come from 5,000 individually timed samples after
    200 warm-up samples.
  - Criterion's median and its 95% interval come from a library-driven
    Criterion run with a 1 s warm-up and 3 s of measurement.
  - `wakeups` counts context switches. On Linux these are voluntary switches
    (`ru_nvcsw`). On Windows they are each thread's `ContextSwitches`, which
    also counts preemptions.
  - `copies` counts payload copies by user code or the kernel, in both
    directions.
  - `clock_reads` counts clock reads inside the transport.
  - Windows CPU from `GetProcessTimes` is sampled at clock ticks, so the
    runner also reports exact `QueryProcessCycleTime` cycles.
  - Cold start is helper spawn to core receiving `READY`, 50 times per
    candidate.
  - The target's and each helper's CPU affinity must equal the runner's.
  - `load_other_busy_cpus` is the busy CPU time that core and helper do not
    account for, divided by wall time. On Linux it covers the runner's
    affinity CPUs; on Windows it covers the whole machine.
- **Paired rounds.** Each platform ran five rounds. Even rounds run the
  candidates in reverse order (`--round N`).

### Linux reference results

These are exploratory figures. The Linux host is the Ryzen 9 9950X3D running
kernel `7.3.5-2-cachyos-bore` and Rust 1.96.1, with the bench pinned by
`taskset -c 0-7,16-23` and the target and helpers inheriting that pinning.
The source is `a63a0bb`. Round 1 ran
`cargo bench --locked --bench brokered_reads_bench -- --round 1` and rounds
2 to 5 ran the same executable directly (`--bench --round N`). The load
average was 21 to 26, and other work kept 2.1 to 6.1 of the 16 pinned CPUs
busy during the candidate sessions. Each value is the median of five rounds,
in nanoseconds; CPU is per round trip.

| Shape | Variant | p50 | p99 | Criterion median [95% CI] | Core CPU | Plugin CPU |
|---|---|---:|---:|---:|---:|---:|
| precise | direct | 7,594 | 14,637 | 7,978 [7,912–8,043] | 8,238 | — |
| precise | uds | 17,253 | 41,869 | — | 14,092 | 4,535 |
| precise | shm | 11,252 | 29,957 | — | 10,987 | 1,744 |
| precise | ring | 14,557 | 41,058 | 15,861 [15,328–16,224] | 12,218 | 3,282 |
| precise | naive | 282,563 | 852,810 | — | 172,289 | 136,594 |
| regular | direct | 3,667 | 7,314 | 3,694 [3,671–3,706] | 3,839 | — |
| regular | uds | 13,355 | 28,493 | — | 8,524 | 4,154 |
| regular | shm | 6,853 | 16,201 | — | 6,195 | 1,708 |
| regular | ring | 9,388 | 20,328 | 10,092 [9,943–10,193] | 7,164 | 2,902 |
| regular | naive | 51,007 | 125,167 | — | 31,023 | 24,756 |

Every brokered variant made one logical submission and carried two wire
frames per sample, with no control frames; `naive` made 43 (precise) or 8
(regular) submissions with twice as many frames. All result hashes matched
the target and `direct` in every round. The `ring`
counters per round trip were the same in every round:

- 1.9 context switches (about one per side);
- 4 payload copies (T0's two per frame);
- 2 clock reads (each send's backpressure deadline);
- 93 (precise) or 41 (regular) allocations, all in core's prototype
  executor (`direct` alone makes 77 and 30); the plugin side made none.

Over the five rounds, `ring` p50 was 1.297× `shm` for precise and 1.383× for
regular. The likely cause, inferred from the code paths and not profiled, is
the T0 ring's per-frame head, policy check, aligned copy and producer lock,
which the prototype's two fixed slots do not have. Cold start for `ring`, from spawn to
`READY`, was 0.49 ms at p50 and 1.03 ms at p99. A first, heavier-loaded set
of rounds on `30bd218` (11 to 15 busy CPUs from other work) gave the same
ordering, at 1.29× and 1.41×.

### Windows results

All Windows runs were native, in the Windows 11 Pro 10.0.26300 guest. The
guest has 16 vCPUs pinned to host CPUs 8–15 and 24–31 of the same Ryzen 9
9950X3D, and Rust is 1.96.1. Each round built a release bench with
`--no-default-features` and ran it directly under the explicit affinity
launcher `cmd /c start "" /B /WAIT /AFFINITY FFFF <bench.exe> --bench --round N`.
Every target and helper reported affinity `0-15`, equal to the runner's.
Other agents shared the VM, so every session records the busy CPUs it could
not attribute to core or helper (`load_other_busy_cpus`). There were three
sets of five paired rounds:

- **A** on `30bd218`. Other load was 0.5–1.9 busy CPUs, except 4.5 in one
  round. This set's plugin CPU figures are not used, because the
  switch-count query then fell inside the measured window (fixed in
  `a63a0bb`).
- **B** on `a63a0bb`. Rounds 2 and 3 had 6.7–15.4 busy CPUs.
- **C** on `a63a0bb`. Rounds 4 and 5 had 8.4–15.6 busy CPUs.

Set C has the final code, so its medians (in nanoseconds) and per-round-trip
counters appear below. The CPU columns come from `GetProcessTimes`, which is
tick-sampled. The cycle column is the median over the five rounds of the
exact `QueryProcessCycleTime` cycles per round trip.

| Shape | Variant | p50 | p99 | Criterion median [95% CI] | Core CPU | Plugin CPU | Core / plugin cycles |
|---|---|---:|---:|---:|---:|---:|---:|
| precise | direct | 16,100 | 40,900 | 17,554 [16,764–18,466] | 18,750 | — | 74,457 / — |
| precise | section | 80,500 | 329,400 | 94,952 [91,808–105,739] | 62,848 | 22,859 | 274,572 / 82,822 |
| precise | pipe | 89,800 | 314,300 | 110,960 [107,509–119,188] | 66,255 | 27,814 | 289,675 / 103,585 |
| regular | direct | 5,600 | 12,300 | 6,311 [6,171–6,421] | 6,250 | — | 27,172 / — |
| regular | section | 65,000 | 205,500 | 75,932 [72,959–78,515] | 48,148 | 19,554 | 204,491 / 76,112 |
| regular | pipe | 71,800 | 303,100 | 93,109 [87,485–95,700] | 59,829 | 28,721 | 235,675 / 95,958 |

Both candidates made one logical submission and carried two wire frames per
sample, with no control frames and about 1.6 context switches (core 0.6,
helper 1.0). Every result hash matched the target and `direct` in every
round of all three sets. Allocations were identical: 94 (precise) or 41
(regular) in core's executor, and none on the plugin side. The carriers
differed only in the transport's own work:

- `section`: 4 payload copies and 2 clock reads per round trip.
- `pipe`: 8 copies (the staging copy, `WriteFile`, `ReadFile` and the
  aligned copy for each frame) and no clock reads.

Cold start from spawn to `READY` was 17.9 ms at p50 and 42.7 ms at p99 for
`section`, and 17.4 ms and 38.8 ms for `pipe`. Set A gave 16.6 and 12.1 ms
at p50.

**Paired comparison under the frozen tie rule.** The pipe would need a lower
p50 in all five paired rounds and a median improvement of at least 10%.

| Set | precise `pipe`/`section` p50, median [rounds pipe lower] | regular, same |
|---|---|---|
| A (`30bd218`) | 1.049 [0/5] | 1.041 [2/5] |
| B (`a63a0bb`) | 1.084 [2/5] | 1.063 [1/5] |
| C (`a63a0bb`) | 1.256 [0/5] | 1.058 [1/5] |
| 8 quiet rounds (every session's other load below 2 CPUs) | 1.034 [1/8] | 1.057 [2/8] |

In the quiet rounds the median `section` p50 was 83.6 µs for precise and
64.9 µs for regular.

### Findings

- **The pipe does not win, and the section stays.** In no set was the pipe
  faster in all five rounds, and its median was 3–26% slower, never faster.
  The Criterion intervals agree: in C both shapes' pipe intervals lie wholly
  above the section's. The pipe also copies twice as much per frame.
- **A Windows round trip costs about 60–75 µs over `direct` in this guest
  for either carrier**, compared with about 6–7 µs for the Linux `ring`. The
  round trip's CPU work is real, not tick-sampling error: core executes
  about 200,000–290,000 cycles per round trip, against 27,000–74,000 for
  `direct`. The wake path probably dominates. This is an inference, not a
  profile: when other agents kept the vCPUs busy, the same `section` round
  trip fell to 42.2 µs (precise) and 11.2 µs (regular), which fits
  cross-vCPU wakeups of halted vCPUs being the expensive part. Bare-metal
  Windows was not measured.
- **Contention distorts the absolute Windows figures in both directions.**
  Busy vCPUs made round trips faster while p99 got worse, so an absolute
  Windows bar from these runs would mostly measure the guest's idle state.
  A same-run ratio does not.

### Decision and Windows bar (approved 2026-11-25)

Main approved this proposal on 2026-11-25 under A5.6. The measurements above
remain the exploratory candidate result (shared CPUs and VM), not D17
evidence: the bar below is gated by a later exclusive run of the production
variants.

1. **Windows transport: keep the unnamed section and auto-reset events**
   (A5.6's default). The pipe fails the frozen tie rule in every set, so it
   stays measurement-only, and no secure-handoff amendment is requested. W0's
   final mechanics can proceed on T0's section and event endpoints.
2. **Windows D17 bar (adopted).** This is A5.6's frozen form with `section` as
   the denominator: for each shape, the median over five paired rounds of
   `production_sync p50 / section p50`, both from the same invocation, is
   at most 2.0. It is a gate only on these conditions:
   - The run uses this VM shape (16 vCPUs, the `/AFFINITY FFFF` launcher,
     every child's affinity verified) and holds the VM exclusively.
   - It records `load_other_busy_cpus` for every session.
   - Any paired round in which another workload kept 2 or more CPUs busy is
     rerun, not counted.
3. **Historical Windows reference.** Report twice this experiment's quiet
   `section` p50 alongside the gate, the way the Linux prototype's 21.540 and
   13.126 µs are: **167.2 µs for precise and 129.7 µs for regular**. It is
   reported, never gates, and never replaces the same-run ratio.
4. **Linux.** The Linux bar is unchanged (`production_sync` at most 2.0×
   same-run `shm`). As data, the T0 ring alone already costs 1.30–1.38× `shm`
   before any relay or executor work is added.
5. **Accepted method deviations.** Main accepted these with the decision:
   the Linux `ring` reference variant (T0's ring over a memfd and an
   eventfd); the prototype variants keeping their original CPU accounting
   method; the `cold_*` names for the separately reported cold-startup
   metrics; `wakeups` meaning each OS's own context-switch count (voluntary
   `ru_nvcsw` on Linux, per-thread `ContextSwitches`, preemptions included,
   on Windows; see Method); and Criterion storage selected
   through `output_directory`.

## Production arm (M5 P1, exploratory)

`production_sync` and `production_async` now run the real transport and
executor: a real `vosu plugin-host` child loads
`examples/brokered_reads_plugin.rs`, whose plans cross the real relay, ring,
ingress and broker against the same target. The first exploratory,
contended Linux run gave a `production_sync` p50 of 2.70× (precise) and
2.26× (regular) the same run's `shm`, above the 2.0 bar. The broker
performance work after it took exploratory same-session ratios to
1.46–1.59× (precise) and 1.71–1.81× (regular). That work batched each
dependency level into one native read, removed the per-request `SYNC`
budget-timer wake and two allocations, and validated `SYNC` plans flat in
the relay. The rest of the gap is the four thread hand-offs that ABI §7.6
requires. These figures come from a loaded, shared host. No D17 result is
claimed, and the official run is pending. The benchmark guide has the
method, gates and numbers, the 25-ABI-stage precise mapping, the per-hop
breakdown, each change's A/B, the bundled `plan_memory` comparison and the
official command sequence:
[M5 P1 production arm](../testing/benchmarks.md#m5-p1-production-arm-and-x1-process-runtime-data-exploratory)
and [broker performance work](../testing/benchmarks.md#broker-performance-work-exploratory-same-session-ab).

## Open questions

- **Windows transport** is decided (unnamed section and events, approved
  2026-11-25); see the [Windows transport
  candidates](#windows-transport-candidates-m5-p1-candidate-result) section
  above.
- **Real captures are many round trips.** X1 counted actual process-runtime
  captures over the bench fixtures: a lazer precise capture is 21
  submissions (26 stages, 60 reads, 912 bytes) and a stable regular-fields
  capture 13 (15 stages, 56 reads, 4,324 bytes), not the one round trip of
  the 5-stage regular estimate above
  ([counts](../testing/benchmarks.md#m5-p1-production-arm-and-x1-process-runtime-data-exploratory)).
  Whether cross-decoder coalescing is needed is open.
