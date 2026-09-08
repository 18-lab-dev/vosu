---
type: Benchmark Guide
title: vosu benchmarks
description: Criterion benchmark scope, reproducible comparison workflow, fixture limitations, and machine-local observations.
tags: [testing, benchmarks, criterion, performance]
---

# Benchmarks

The repository has one Criterion benchmark target, `hot_paths`, declared with
`harness = false` in `Cargo.toml`. It is a developer measurement tool, not a CI
quality gate: `.github/workflows/ci.yml` checks, tests, and builds all targets,
but does not run `cargo bench`.

## Scope

Names below are the complete benchmark IDs from `benches/hot_paths.rs`.

| Group | Benchmarks | What is measured |
| --- | --- | --- |
| `batch_scan` | `three_patterns_2mib` | Scanning one 2 MiB in-memory region for three patterns. Throughput is declared as 2 MiB per iteration. |
| `raw_snapshot_reads` | `stable_regular_fields`, `lazer_regular_fields` | Calls to `global`, `menu`, and `gameplay` through the stable and lazer memory readers. |
| `linux_process_memory` | `current_process_64_bytes` | Linux-only `process_vm_readv` of 64 bytes from the benchmark process through the production `ProcessMemory` reader. Throughput is declared as 64 bytes per iteration. |
| `windows_process_memory` | `read_exact_64_bytes`, `read_many_single_64_bytes`, `read_many_contiguous_8x8_bytes`, `read_many_gapped_8x8_bytes`, `read_many_wide_8x8_bytes` | Windows-only current-process `ReadProcessMemory` calls through the production `ProcessMemory` reader. `read_many` combines ordered non-overlapping requests when the full span fits one 4 KiB page; spans up to 1 KiB use fixed stack scratch, while larger same-page spans use allocation-backed scratch. Singleton requests bypass batching scratch. Throughput is declared as 64 bytes per iteration. |
| `map_calculation` | `parse`, `full_difficulty_performance`, `gradual_all_objects`, `unchanged_cache_key` | Parsing, difficulty plus performance calculation, gradual difficulty, and an unchanged-key cache hit for the embedded five-object osu!standard map. Throughput is declared as five elements per iteration. |
| `serializers_cold` | `v1`, `v2`, `v2_precise`, `stream_companion` | Encoding a newly constructed synthetic API snapshot. The precise fixture carries 18 ordered mania keypresses. Snapshot construction is batched outside the timed routine. |
| `serialization_cache` | `v1_bytes_clone`, `v2_bytes_clone`, `v2_precise_bytes_clone`, `stream_companion_bytes_clone` | Re-encoding the same pre-warmed snapshot, exercising each encoder's cached `Bytes` return path. |
| `publication` | `unfiltered_clients/1`, `unfiltered_clients/100`, `unfiltered_clients/1000` | One in-memory publication delivered to and acknowledged by 1, 100, or 1,000 unfiltered hub connections on a current-thread Tokio runtime. Throughput is declared as the client count. |
| `v2_subscription` | `shared_plan/1`, `shared_plan/100`, `shared_plan/1000`, `distinct_plans/1`, `distinct_plans/100`, `distinct_plans/1000` | One due revision dispatched through the public selected-field hub/provider APIs to 1, 100, or 1,000 in-memory connections. Shared clients use one identical ordered plan; distinct clients each use a different ordered plan. Throughput is declared as the client count. |

## Running and selecting benchmarks

The workspace pins Rust 1.96.1 in `rust-toolchain.toml`; Cargo uses the
optimized bench profile. Run all benchmarks from the repository root:

```sh
cargo bench --locked --package vosu --bench hot_paths
```

The corrected `publication` fixture uses a 1 ms poll interval and completes under
Criterion's default sampling; see the current local observation below.

Criterion 0.7.0 accepts a positional substring filter after Cargo's `--`:

```sh
cargo bench --locked --package vosu --bench hot_paths -- raw_snapshot_reads
cargo bench --locked --package vosu --bench hot_paths -- serializers_cold/v2
cargo bench --locked --package vosu --bench hot_paths -- linux_process_memory
```

Run only the selected-field subscription dispatch cases with:

```sh
cargo bench --locked --package vosu --bench hot_paths -- v2_subscription
```

The second example matches both `serializers_cold/v2` and
`serializers_cold/v2_precise`. Select only one full benchmark ID with
`--exact`:

```sh
cargo bench --locked --package vosu --bench hot_paths -- serializers_cold/v2 --exact
```

Keep filters identical when comparing runs: a partial run updates or compares
only the matching benchmark directories.

### Named baselines

The current Windows read-many implementation merges ordered requests within one 4 KiB page, uses fixed stack scratch through 1 KiB, and switches to allocation-backed scratch for larger same-page spans; singleton requests bypass batching scratch. A 3,368-byte wide shape is covered by `read_many_wide_8x8_bytes` and the Windows read tests. This is a native-reader implementation detail, not evidence that synthetic current-process benchmarks model live cross-process reads.

A plain Criterion run uses and overwrites the machine-local baseline named
`base`. Prefer a descriptive named baseline for regression work:

```sh
cargo bench --locked --package vosu --bench hot_paths -- raw_snapshot_reads --save-baseline before-change
cargo bench --locked --package vosu --bench hot_paths -- raw_snapshot_reads --baseline before-change
```

`--baseline` is strict and fails if any selected benchmark lacks that baseline.
For an intentionally changed benchmark set, Criterion 0.7.0 also supports:

```sh
cargo bench --locked --package vosu --bench hot_paths -- raw_snapshot_reads --baseline-lenient before-change
```

Baselines live under `target/criterion/`; they are build artifacts, not portable
records. `--discard-baseline` runs measurements without retaining them. Useful
Criterion controls include `--sample-size`, `--warm-up-time`,
`--measurement-time`, `--nresamples`, `--noise-threshold`,
`--confidence-level`, and `--significance-level`. Record any override and use
exactly the same values on both sides of a comparison.

## Reproducible method

Criterion 0.7.0 defaults to 100 samples, 3 seconds of warm-up, 5 seconds of
measurement, 100,000 bootstrap resamples, a 95% confidence level, a 5%
significance level, and a 1% noise threshold. The benchmark source does not
override these settings.

For a defensible comparison:

1. Use the pinned Rust toolchain and the same checkout, target triple, machine,
   power profile, CPU affinity policy, and benchmark filter for both runs.
2. Stop competing CPU-, memory-, and I/O-intensive work. Let the system reach a
   stable temperature and avoid comparing runs with different thermal or boost
   behavior.
3. Save a named baseline before the code change, make the change, then compare
   with strict `--baseline`. Do not compare results copied from another host.
4. Treat Criterion's confidence interval and change test together. A lower
   point estimate alone is not evidence of improvement; investigate outliers,
   repeat noisy runs, and require a practically meaningful effect as well as a
   statistically detected one.
5. If a result regresses, rerun that exact benchmark ID, inspect the changed hot
   path, and repeat the strict comparison. Keep the same Criterion controls;
   increasing measurement time or sample size is appropriate only when applied
   consistently to both measurements.

## Fixture limits

These benchmarks isolate code paths; they are not end-to-end workload models.
In particular:

- `raw_snapshot_reads` uses `SliceMemory`, which reads from an `Arc<[u8]>` and
  copies slices in the benchmark process. `read_many` is a loop over those
  copies and `readable_regions` returns one synthetic region. It does not
  measure OS process-memory reads.
- The Linux-only `linux_process_memory` case reads 64 bytes from the benchmark
  process through the production `ProcessMemory` implementation. It measures
  the real `process_vm_readv` syscall and reader overhead, but not cross-process
  permissions, page faults, a large request batch, or a live osu! object graph.
- The Windows-only `windows_process_memory` cases call production `ProcessMemory` against the current benchmark process through native `ReadProcessMemory`: `read_exact_64_bytes` is one exact 64-byte request, while `read_many` covers singleton, contiguous, gapped, and greater-than-1-KiB wide ordered non-overlapping requests in a synthetic `#[repr(align(4096))]` page. They measure current-process native syscalls and synthetic aligned same-page layouts, not cross-process faults or a live osu! process. Production `read_many` bypasses batching scratch for singleton requests; otherwise spans up to 1 KiB use fixed stack scratch and larger same-page spans use allocation-backed scratch, with merging capped at one 4 KiB page. The reader retries unread suffixes after short reads, rejects a false backend result even when the byte count equals expected, and zeroes all destinations if any read fails.
- Snapshot reads use a zero-filled 64 KiB fixture, placeholder stable addresses,
  and pre-resolved synthetic lazer addresses. They do not model realistic
  object graphs or changing game state.
- Pattern scanning uses one zero-filled 2 MiB region with three inserted
  markers. Match density, region layout, and bytes differ from a live process.
- Map calculation uses one embedded, five-object osu!standard map with default
  live difficulty configuration and a fixed perfect score. It does not cover
  large maps, other rulesets, varied mods, malformed input, or disk I/O.
- Serializer inputs are synthetic. `serializers_cold` measures encoding but not
  snapshot construction; `serialization_cache` deliberately measures the warm
  cached-byte clone path rather than a cache miss.
- Publication uses an in-memory provider, receivers, and a generation-number
  payload. It excludes TCP, HTTP upgrade, WebSocket framing, network backpressure,
  client parsing, and real schema serialization.
- V2 subscription dispatch uses a current-thread Tokio runtime, public
  `V2SubscriptionHub`/provider APIs, and in-memory bounded receivers. Every case
  materializes the same synthetic document containing a changing revision and
  a fixed 1,000-element array. `shared_plan` clients all select the ordered
  `["/revision", "/clients/0"]` plan; client `i` in `distinct_plans` selects
  `["/clients/i", "/revision"]`. All subscriptions are installed before the hub
  task starts.
- Each timed V2 subscription iteration publishes one due revision, waits for
  one text frame per client, asserts exactly one document build, and marks every
  write complete. Fixture setup separately verifies that publishing while every
  client is pending does not build a document, then verifies catch-up after
  writes reopen. The timed result includes provider publication, synthetic
  document construction, selection, frame encoding, bounded-channel delivery,
  receiver draining, counter assertions, and write completion. It excludes
  TCP, HTTP/WebSocket framing, browser parsing, socket backpressure, and a real
  `/json/v2` snapshot.
- No allocator instrumentation was enabled for the V2 subscription run.
  Allocations performed by document construction and dispatch contribute to
  elapsed time, while connection, plan, runtime, and initial-frame setup occur
  outside the timed iterations. There are no allocation-count or byte-count
  observations.

## Memory-reader comparison with upstream tosu: 2026-09-03

These are **machine-local observations, not performance promises, acceptance
thresholds, or portable baselines**. The comparison used vosu revision
`76e3d32a242e` and upstream tosu v4.26.1 at revision
`1b19ddaf04b243efb522b8db704849d01b0a82ce` on an AMD Ryzen 9 9950X3D host.

### Isolated deterministic reader workload

Seven independent invocations exercised an equivalent active-lazer
precise-capture cycle through each implementation's production process-memory
backend. Each invocation included warm-up and nine internal timing samples. The
cycle resolved the game base and current screen, then read precise global state,
the 18-key overlay, and hit errors.

| Metric | Upstream tosu | vosu | Difference |
| --- | ---: | ---: | ---: |
| Median latency per cycle | 31,928.23 ns | 8,112.73 ns | vosu **3.94x faster** |
| Process-memory operations per cycle | 156 | 29 | vosu **81.4% fewer** |
| Requested bytes per cycle | 1,020 B | 570 B | vosu **44.1% fewer** |

The reduction comes from batching the fixed 18-key trigger array and related
key-state fields, validating the game base once per synchronized capture, and
reusing bounded capture-local pointer state. This workload uses self-process
fixtures to make native reads deterministic. It excludes process discovery,
polling orchestration, state projection, serialization, HTTP/WebSocket
publication, cross-process faults, and a changing live osu! object graph.

### GPU-backed active-gameplay whole-daemon workload

A separate Windows 11 guest comparison had both daemons simultaneously read the
same osu!lazer 2026.726.0 process while it played Renatus `[Another]`, a 7K
osu!mania map (`Mode: 3`, `CircleSize: 7`), with Autoplay (`AT`). The guest
exposed eight logical processors and an NVIDIA GeForce RTX 5090. tosu listened
on `127.0.0.1:24050`; vosu listened on `127.0.0.1:24051`.

Three 45-second trials issued 900 alternating `/json/v2` requests to each
daemon per trial. All 2,700 requests per daemon succeeded and remained in active
play. Values below are medians across the three trial-level measurements.

| Metric | Upstream tosu | vosu | Difference |
| --- | ---: | ---: | ---: |
| CPU time per 45-second trial | 2,250 ms | 1,390.625 ms | vosu **38.2% lower** |
| Median HTTP latency | 3.0157 ms | 2.6578 ms | vosu **11.9% lower** |
| HTTP latency p95 | 6.9458 ms | 6.6257 ms | vosu **4.6% lower** |
| Mean private memory | 128.14 MiB | 11.98 MiB | vosu **90.7% lower** |

“GPU-backed” describes the rendered active-gameplay environment, not a GPU
benchmark: no GPU utilization, power, or frame-time samples were collected.
This workload measures loaded whole-daemon CPU, memory, projection,
serialization, and HTTP behavior. It does not isolate memory-reader cost because
the daemons share and perturb the same guest. Use the deterministic workload
above—not this simultaneous daemon run—as the basis for isolated reader
performance claims.

## End-to-end comparison with upstream tosu: 2026-07-26

This is a **non-portable, machine-specific observation, not a performance
promise, acceptance threshold, or portable baseline**. Every `tosu` label in
this section refers explicitly to upstream tosu v4.25.1.

### Environment, revisions, and trial design

- Host: AMD Ryzen 9 9950X3D and NVIDIA GeForce RTX 5090.
- Guests: sequential Windows and Linux virtual machines, each with 8 logical
  CPUs.
- Revisions: vosu `fed2fc71f35238e013feff44add60c9256768213`,
  vosulay `8249a39ff7dc05365f59e47402d9945212bda3cd`, upstream
  tosu v4.25.1, and osu!lazer 2026.726.0.
- Design: three counterbalanced pairs per suite. Daemon and overlay trials used
  30 seconds of warm-up followed by 120 seconds of measurement and an exact
  10 Hz validator (1,201 requests). WebSocket trials used a 30-second
  measurement window and an independent exact 10 Hz HTTP validator (301
  requests).
- Workload: the same Renatus/Autoplay gameplay for both products in a pair.
  The Linux daemon comparison used osu!standard because upstream tosu's Linux
  Mania reader repeatedly emitted performance-cast errors.

The two kinds of statistics below must not be mixed. An **absolute** entry is
the marginal median of the three per-product trial means. A **paired effect** is
the median of the three relative comparisons calculated within matched,
counterbalanced vosu/upstream-tosu trials. Consequently, the paired effects
must not be derived from the two absolute columns.

### Daemon and full `/json/v2` observations

#### Windows guest

| Metric | vosu absolute median | Upstream tosu absolute median | Median paired vosu effect |
| --- | ---: | ---: | ---: |
| CPU (% of one logical CPU) | 3.906627% | 3.815343% | 2.392537% higher |
| Process-tree private memory | 10.518369 MiB | 104.724077 MiB | 89.956112% lower |
| Process-tree working set | 39.954642 MiB | 119.597947 MiB | 65.861563% lower |
| Full `/json/v2` payload | 22853.669421 B | 20991.446281 B | 9.003864% larger |

#### Linux guest

| Metric | vosu absolute median | Upstream tosu absolute median | Median paired vosu effect |
| --- | ---: | ---: | ---: |
| CPU (% of one logical CPU) | 4.374997% | 2.666665% | 61.556865% higher |
| Process-tree private memory | 15.453545 MiB | 238.606308 MiB | 93.506964% lower |
| Summed process-tree RSS | 18.772437 MiB | 248.356308 MiB | 92.410729% lower |
| Full `/json/v2` payload | 50311.148760 B | 49395.719008 B | 1.853257% larger |

vosu's modestly larger full `/json/v2` sample is expected partly from additive
schema data. The current schema adds `beatmap.stats.timingWindows`, and
`play.hitErrorArray` is structured as `{all, press, release}`. Values in
`press` can repeat timing values also present in `all`, so this contribution to
the size difference can grow as more objects are completed. This is schema
enrichment, not evidence of serializer inefficiency. These fields are known
contributors, not a claim that they are the only possible byte difference.

Consumers that do not need the full document can use selected-field
subscriptions, avoiding transmission of unused fields. The following
observation requested only `/beatmap/time/live` and
`/play/hitErrorArray/all` from vosu, while upstream tosu delivered its full
snapshots.

### Selected-field WebSocket observations

#### Windows guest

| Metric | vosu selected fields: absolute median | Upstream tosu full snapshots: absolute median | Median paired vosu effect |
| --- | ---: | ---: | ---: |
| Delivered frames/s | 59.750570 | 6.550088 | 9.124709x cadence |
| Frame interarrival p50 | 16.378650 ms | 152.622150 ms | 89.267144% lower |
| Frame interarrival p95 | 19.235875 ms | 154.937775 ms | 87.584774% lower |
| Decoded JSON/s | 4200.266667 B/s | 127292.966667 B/s | 96.700135% lower |
| Mean decoded frame | 70.324038 B | 19384.715736 B | 99.637219% smaller |

#### Linux guest

| Metric | vosu selected fields: absolute median | Upstream tosu full snapshots: absolute median | Median paired vosu effect |
| --- | ---: | ---: | ---: |
| Delivered frames/s | 99.998219 | 6.634251 | 15.079610x cadence |
| Frame interarrival p50 | 10.114817 ms | 150.506520 ms | 93.281605% lower |
| Frame interarrival p95 | 12.157455 ms | 152.502563 ms | 92.028032% lower |
| Decoded JSON/s | 3033.933333 B/s | 324895.266667 B/s | 99.066474% lower |
| Mean decoded frame | 30.422667 B | 48748.381910 B | 99.937592% smaller |

Frame interarrival is the time between deliveries at the receiver. It measures
delivery cadence, not one-way latency. Decoded JSON/s counts decoded JSON
payload bytes and is not wire bandwidth; WebSocket framing, transport details,
and the differing selected-field versus full-snapshot semantics prevent that
interpretation.

### Overlay paired-delta context

A separate overlay exercise used the primary paired ON-OFF medians. `ON` was
the game plus its required daemon plus the renderer/host; `OFF` was the same
game plus its required daemon with the renderer/host disabled. Each value
below is the median paired full-stack increase with the overlay enabled, not an
absolute resource level. The delta captures all induced work, including work
charged outside renderer processes. Both overlays rendered the same HTML at
1280×720.

| Metric | Upstream tosu on Windows, ON − OFF | vosulay on Linux, ON − OFF |
| --- | ---: | ---: |
| CPU | +31.114192 one-core percentage points (+3.8893% of 8-vCPU capacity) | +14.000311 one-core percentage points (+1.7500% of 8-vCPU capacity) |
| Process-tree private memory | +384.654894 MiB | +412.209097 MiB |
| Resident-memory measure | +498.781637 MiB working set | +1121.972011 MiB summed RSS |
| GPU utilization | +12.214876 percentage points | +2.520661 percentage points |
| VRAM | +19.181818 MiB | +107.512397 MiB |
| GPU power | — | +11.568760 W |

Windows and Linux use different display stacks and frame policies. These
side-by-side overlay deltas are directional context only, not a
platform-neutral contest or evidence of a cross-platform winner.

### Limitations

- This was one host, two specific virtualized guests, one set of revisions,
  three trial pairs, and one gameplay scenario. The observations are
  descriptive and should not be generalized to other machines, guest
  configurations, maps, rulesets, sessions, or releases.
- CPU percentage uses one logical CPU as 100%; each guest had 8 logical CPUs.
  Process-tree memory sums can double-count shared pages, so private memory,
  Windows working set, and Linux summed RSS are not interchangeable measures.
- The Linux ruleset exception was required by repeated errors in upstream
  tosu's Linux Mania reader and limits direct ruleset generalization.
- Full `/json/v2` payload sizes include schema and live-content differences,
  not just encoding work. The selected-field comparison intentionally carries
  different payload semantics from upstream tosu's full snapshots.
- Interarrival describes receiver-side delivery cadence rather than one-way
  latency, and decoded JSON byte rate is not network bandwidth.
- Overlay resource deltas are workload-, renderer-, display-stack-, and
  frame-policy-dependent. In particular, the Windows upstream-tosu and Linux
  vosulay columns must not be treated as an OS-normalized product comparison.

## Follow-up upstream comparison: 2026-07-28

This is a **non-portable, machine-specific observation, not a performance
promise, acceptance threshold, or portable baseline**. It records a follow-up
Linux daemon comparison and a separate host-browser dashboard comparison.
Every `tosu` label below refers explicitly to upstream tosu v4.25.1.

### Linux daemon CPU and memory

The tested vosu revision was
`02c688424ad4215238c52911dac539ebaad0c41d`. The exact production build
command was:

```sh
cargo build --release --locked --no-default-features --target x86_64-unknown-linux-gnu
```

The vosu binary had SHA-256
`3318f9456b9ebf02a680cc9da445e5351f5aeac28236db0fe64b3b8e570c10c9`;
the official tosu v4.25.1 binary had SHA-256
`8472f135528d89c0a03de09f52dfdf890bb71f1df46e4d03ee067d1950163e39`.
Both ran as the unprivileged `vosu` user with `cap_sys_ptrace=eip`. Only the
process-tree sampler ran privileged so it could read `smaps_rollup`;
`ptrace_scope` was not lowered.

Both configurations used 150 ms normal and 10 ms precise polling, port 24050,
and disabled update installation, dashboards, and in-game overlays. Each
product completed three counterbalanced trials. Every trial had a 30-second
warmup followed by 120 seconds of measurement, `/json/v2` scheduled at 10 Hz
(1,201 successful requests), and process-tree CPU/private/RSS sampling at
1 Hz. Before every trial, the API had to report a progressing active snapshot
from osu!lazer 2026.726.0 running Renatus [Insane], osu!standard, with
Autoplay.

Headline values below are marginal medians of the three trial-level
duration-weighted CPU means or mean-memory values. The reduction column is
calculated from those two absolute medians; it is not a paired-effect
statistic.

| Metric | vosu absolute median | Upstream tosu absolute median | Reduction from upstream absolute median |
| --- | ---: | ---: | ---: |
| CPU (% of one logical CPU) | 1.175% | 1.867% | 37.1% lower |
| Process-tree private memory | 15.12 MiB | 230.06 MiB | 93.4% lower |
| Summed process-tree RSS | 18.46 MiB | 239.67 MiB | 92.3% lower |

The result repeated without overlapping trial ranges:

- CPU: vosu 1.158–1.192%; upstream tosu 1.833–1.867%.
- Private memory: vosu 14.88–16.68 MiB; upstream tosu
  229.56–241.65 MiB.
- RSS: vosu 18.22–20.14 MiB; upstream tosu 239.17–251.28 MiB.

The current vosu build therefore beat this upstream release in both requested
daemon resource dimensions without another source change. Raw CSVs, active
workload gates, launch provenance, harness, and analysis are under
`/home/user/VMs/benchmarks-2026-07-28/linux/`. `summary.json` has SHA-256
`1b4c9289bf1fd8f72e8d14d3effec1dfbd89a066fd8f71eff55c30f999915ea1`.

### Memory-to-client propagation latency

This experiment compared vosu's forced polling, `CN_PROC`, and eBPF
process-event modes with official tosu v4.25.1. It measures the already
attached telemetry path, not process-discovery latency. The game was already
running before each daemon started, so the selected process-event backend
could affect discovery and attachment but was not expected to change vosu's
post-attachment memory-read or WebSocket publication path.

The tested vosu artifact was built from revision
`02c688424ad4215238c52911dac539ebaad0c41d` with the repository's canonical
production command:

```sh
cargo build --release --locked --no-default-features --features linux-process-events --target x86_64-unknown-linux-gnu
```

The measured binary had SHA-256
`2a2ef22235d184b7071fdee13057bf6b3b86cb8e57c87d9adcd457f32c3e49d3`.
The official tosu v4.25.1 binary had SHA-256
`8472f135528d89c0a03de09f52dfdf890bb71f1df46e4d03ee067d1950163e39`.
The same 8-vCPU Linux guest used above ran kernel `7.1.4-zen1-1-zen`.
Vosu ran with the exact forced values `poll`, `cn-proc`, and `ebpf`.
`CN_PROC` and eBPF passed an unprivileged backend-open probe before every
trial, every daemon log named the requested backend exactly once, and no
fallback, source-failure, or reconciliation marker appeared. Official tosu's
internal process backend is opaque, so no backend attribution is claimed for
its column.

Four trials per variant used a cyclic 4×4 Latin square. After an exact workload
gate and 10-second warm-up, an independent direct-memory observer sampled
osu!lazer at an absolute 1 ms cadence while a Node client received the common
full `/websocket/v2` stream. The workload was continuously required to remain
osu!lazer 2026.726.0, Renatus [Insane], osu!standard, exact `AT`-only
Autoplay, with the pinned beatmap and mods checksums. The 30-second source
window was followed by a 1-second client drain.

The primary event is a newly observed cumulative hit-error index and exact
value. Its latency is:

```text
client callback CLOCK_MONOTONIC ns - direct-memory read_end CLOCK_MONOTONIC ns
```

Every source event had to appear at the same cumulative index and value in the
first qualifying client frame. A skipped direct-observer count, unmatched or
right-censored event, value mismatch, negative latency, clock-calibration
failure, workload change, or target-PID change rejected the trial. All 16
trials passed. Each variant contributed 584 primary events. The secondary
metric uses first crossings of 500 ms `beatmap.time.live` thresholds; it is a
continuous-field proxy rather than a discrete source event.

| Variant | Primary pooled p50 | Primary pooled p95 | Primary pooled p99 | Primary pooled mean | Secondary pooled p50 | Secondary pooled p95 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| vosu polling | 81.784 ms | 147.901 ms | 156.381 ms | 81.388 ms | 79.761 ms | 145.319 ms |
| vosu `CN_PROC` | 75.616 ms | 145.388 ms | 154.514 ms | 79.237 ms | 75.254 ms | 145.342 ms |
| vosu eBPF | 77.370 ms | 146.840 ms | 154.248 ms | 79.775 ms | 78.729 ms | 145.762 ms |
| Official tosu v4.25.1 | 80.721 ms | 148.568 ms | 155.119 ms | 83.399 ms | 79.857 ms | 146.082 ms |

The primary p50 values span 6.168 ms and the p95 values span 3.180 ms across
all four variants, against a configured 150 ms normal publication interval.
`CN_PROC` recorded the lowest pooled p50 and p95. Its four trial p50 values
ranged from 74.258 to 80.072 ms, while the ranges for polling, eBPF, and
official tosu were 78.390–83.679, 75.563–80.898, and 79.456–84.996 ms,
respectively. These overlapping ranges and small, non-monotonic differences
do not establish a latency winner. In particular, this experiment found no
causal post-attachment propagation-latency advantage from `CN_PROC` or eBPF.
A separate process-start experiment would be required to compare
discovery/attachment wake-up latency.

The same 31 boundary snapshots and 30 capture-end CPU intervals per trial
provided resource context. CPU is percent of one logical CPU; private and RSS
are sums over the live daemon process tree.

| Variant | Pooled CPU mean | Pooled CPU p95 | Pooled private mean | Pooled RSS mean |
| --- | ---: | ---: | ---: | ---: |
| vosu polling | 1.083% | 2.001% | 15.407 MiB | 19.056 MiB |
| vosu `CN_PROC` | 1.067% | 2.001% | 15.359 MiB | 19.056 MiB |
| vosu eBPF | 1.067% | 2.000% | 15.567 MiB | 19.381 MiB |
| Official tosu v4.25.1 | 1.533% | 2.002% | 226.955 MiB | 236.660 MiB |

Every observer produced exactly 35,000 rows. Across trials, the worst
trial-level p95 observer dispatch lateness was 0.083 ms and the worst p95
direct-read duration was 0.015 ms. Node-to-`CLOCK_MONOTONIC` calibration had a
worst maximum round-trip of 2.662 ms and a corresponding maximum uncertainty
of 1.331 ms. All primary and secondary diagnostic sets were empty.

This is direct-memory-**observation**-to-client-receipt latency. It is not the
unknown physical field-mutation time, process-discovery latency, one-way
server latency, or WebSocket cadence in isolation. The 1 ms observer can see a
mutation after it occurred, and product-specific frame production,
serialization, scheduling, and correlated cumulative frames remain
confounders. Comparisons are descriptive and do not prove a causal backend
effect.

Raw CSVs, daemon logs, exact workload gates, launch provenance, frozen
harness, strict analyzer, and report are under
`/home/user/VMs/benchmarks-2026-07-28/backend-latency/`.
`summary.json` has SHA-256
`ca81a4ea905b3561d5b014c135633cfa2301157a937ade46f759fd777076a542`;
`report.md` has SHA-256
`8ac7196a7a447ce641cf0d7da9d1047965f25d723c61e50ee0219bdbe0c6159c`;
the 135-file `SHA256SUMS` manifest has SHA-256
`b9f51b259fe7008b378698b274b1d069a065a78add4ee6210acc6e1ad94b9980`.

### Dashboard browser workload

This workload measures browser behavior separately from daemon CPU and memory.
Chrome 150 loaded each dashboard on the Linux host at 1280×800 with the same
one-counter, 640×360 static iframe fixture. Ten trials per product used a
fresh browser context, disabled and cleared cache, and alternated which
product ran first. Readiness required the installed counter card and its
iframe document to be complete. Chrome DevTools Protocol metrics then covered
a 10-second steady window.

| Lower-is-better metric | vosu median (p95) | Upstream tosu median (p95) |
| --- | ---: | ---: |
| Local fixture ready | 60.25 ms (61.0) | 27.40 ms (28.9) |
| Navigation response end | 1.20 ms (1.4) | 1.80 ms (4.3) |
| Window load | 55.40 ms (56.4) | 24.50 ms (26.3) |
| First contentful paint | 36.0 ms (36.0) | 24.0 ms (32.0) |
| Largest contentful paint | 36.0 ms (36.0) | 34.0 ms (40.0) |
| First-party requests | 5 (5) | 10 (10) |
| First-party encoded transfer | 484.18 KiB (484.18) | 414.69 KiB (414.69) |
| JavaScript heap after 10 seconds | 2.580 MiB (2.580) | 2.398 MiB (2.398) |
| Main-thread task time during 10 seconds | 1.919 ms (2.023) | 76.134 ms (83.790) |
| Style recalculation during 10 seconds | 0 ms (0) | 11.436 ms (12.747) |

Upstream tosu reached the local dashboard sooner and used less transfer and
JavaScript heap. vosu used half as many first-party requests and 97.5% less
total main-thread task time in the steady window. The LCP distribution was
mixed: upstream tosu had the lower median, while vosu had the lower p95. The
comparison therefore does not support calling either dashboard uniformly
faster.

Upstream tosu also made one unconditional request per trial to
`https://tosu.app/api.json`, with a median 6.29 KiB encoded response. That
request is excluded from the first-party transfer row and reported separately
so unavailable or cached remote catalog behavior cannot masquerade as local
dashboard performance. vosu made no external request.

Raw per-trial timings and request records are in
`/home/user/VMs/benchmarks-2026-07-28/dashboard/results.json` with SHA-256
`c4ab228b56b8380e25dda4be27cd37da84d3329c6a34e132b26ecb47763ace60`.
The browser harness is archived beside it as `run-dashboard-benchmark.js`.

## Local V2 subscription observation: 2026-07-16

This is a **non-portable observation, not a performance promise, acceptance
threshold, or comparison with a baseline**.

- Command: `cargo bench --locked --package vosu --bench hot_paths -- v2_subscription`
- Host: AMD Ryzen 9 9950X3D, 16 cores / 32 threads, 123 GiB RAM
- OS: x86_64 Linux 7.1.3-1-cachyos-bore
- Compiler: `rustc 1.96.1 (31fca3adb 2026-06-26)`
- Criterion: 0.7.0 defaults listed above; Plotters backend (gnuplot unavailable)
- Timestamp after completion: 2026-07-16T23:44:43-04:00
- Allocation observation: no allocation profiler or counting allocator was
  used; only elapsed time was measured.

The table reports Criterion's arithmetic-mean point estimate and 95% confidence
interval from the `new/estimates.json` files written by this run.

| Benchmark ID | Mean | 95% confidence interval |
| --- | ---: | ---: |
| `v2_subscription/shared_plan/1` | 7.719 µs | 7.705–7.734 µs |
| `v2_subscription/shared_plan/100` | 45.249 µs | 45.228–45.271 µs |
| `v2_subscription/shared_plan/1000` | 378.374 µs | 377.694–379.115 µs |
| `v2_subscription/distinct_plans/1` | 8.004 µs | 7.996–8.012 µs |
| `v2_subscription/distinct_plans/100` | 68.774 µs | 68.737–68.815 µs |
| `v2_subscription/distinct_plans/1000` | 707.751 µs | 707.147–708.353 µs |

## Local 18-key precise serialization observation: 2026-07-18

This is a **non-portable observation, not a performance promise or acceptance
threshold**.

- Command: `cargo bench --locked --package vosu --bench hot_paths -- serializers_cold/v2_precise`
- Host: AMD Ryzen 9 9950X3D, 16 cores / 32 threads, 123 GiB RAM
- OS: x86_64 Linux 7.1.3-1-cachyos-bore
- Compiler: `rustc 1.96.1 (31fca3adb 2026-06-26)`
- Criterion: 0.7.0 defaults; Plotters backend (gnuplot unavailable)

With the synthetic precise snapshot populated by all 18 mania inputs, Criterion
reported `2.0756 µs` with a `2.0365–2.1139 µs` 95% confidence interval. Its
stored-baseline comparison detected no statistically significant performance
change (`p = 0.72`).

## Local publication observation: 2026-07-19

This is a **non-portable observation, not a performance promise or acceptance
threshold**.

- Command: `cargo bench --locked --package vosu --bench hot_paths -- publication`
- Host: AMD Ryzen 9 9950X3D, 16 cores / 32 threads, 123 GiB RAM
- OS: x86_64 Linux 7.1.3-1-cachyos-bore
- Criterion: 0.7.0 defaults; Plotters backend (gnuplot unavailable)
- Wall time: 28.806 s

With the corrected 1 ms fixture poll interval, all three client counts completed:

| Benchmark ID | Mean | 95% confidence interval |
| --- | ---: | ---: |
| `publication/unfiltered_clients/1` | 2.0552 ms | 2.0531–2.0569 ms |
| `publication/unfiltered_clients/100` | 2.0568 ms | 2.0510–2.0624 ms |
| `publication/unfiltered_clients/1000` | 2.1035 ms | 2.0938–2.1130 ms |

## Local Linux process-memory comparison: 2026-07-21

This is a **non-portable observation, not a performance promise or acceptance
threshold**.

- Baseline source: `b25cd52`, with only the identical benchmark fixture added.
- Patched source: `2d7f949`, with the same benchmark fixture.
- Commands:
  `cargo bench --locked --package vosu --bench hot_paths -- linux_process_memory --save-baseline before-maps-fast-path`
  and the corresponding strict `--baseline before-maps-fast-path` run.
- Both runs used the same `/tmp/vosu-criterion-target` target and Criterion
  directory on the same machine.
- Host: AMD Ryzen 9 9950X3D, 16 cores / 32 threads, 123 GiB RAM
- OS: x86_64 Linux 7.1.4-1-cachyos-bore
- Compiler: `rustc 1.96.1 (31fca3adb 2026-06-26)`
- Criterion: 0.7.0 defaults; Plotters backend (gnuplot unavailable)

The table reports Criterion's arithmetic-mean point estimate and 95% confidence
interval:

| Source | Mean | 95% confidence interval |
| --- | ---: | ---: |
| Before: parse `/proc/<pid>/maps` before the read | 8.2092 µs | 8.1961–8.2228 µs |
| After: direct read with mapping-aware fallback | 189.592 ns | 189.315–189.952 ns |

The strict comparison reported a 97.690% mean-time reduction
(`p = 0.00 < 0.05`), or approximately 43.3 times the throughput for this
single-read fixture.

## Local Windows process-memory comparison: 2026-07-22

This is a **non-portable observation, not a performance promise or acceptance
threshold**. These machine-local results do not generalize to other Windows
versions, hardware, virtualization settings, or workloads.

- Commands, in order:

  ```sh
  cargo bench --locked --package vosu --bench hot_paths -- windows_process_memory --save-baseline windows-read-many-before
  cargo bench --locked --package vosu --bench hot_paths -- windows_process_memory --baseline windows-read-many-before
  ```
- Baseline and patched runs used the same guest, workspace, target directory,
  and Criterion directory, with the exact `windows_process_memory` filter.
- Guest: QEMU/KVM Windows 11 IoT Enterprise LTSC Evaluation 10.0.26100, with
  8 logical processors and 16 GiB RAM.
- Host CPU: AMD Ryzen 9 9950X3D
- Compiler: Rust 1.96.1
- Criterion: 0.7.0 defaults; Plotters backend

The table reports Criterion's arithmetic-mean point estimate and 95% confidence
interval:

| Benchmark ID | Baseline mean | Baseline 95% confidence interval | Patched mean | Patched 95% confidence interval |
| --- | ---: | ---: | ---: | ---: |
| `windows_process_memory/read_exact_64_bytes` | 376.25 ns | 375.97–376.51 ns | 377.29 ns | 377.04–377.58 ns |
| `windows_process_memory/read_many_single_64_bytes` | 406.34 ns | 406.05–406.64 ns | 392.52 ns | 392.24–392.83 ns |
| `windows_process_memory/read_many_contiguous_8x8_bytes` | 422.63 ns | 422.45–422.81 ns | 429.25 ns | 412.84–453.37 ns |
| `windows_process_memory/read_many_gapped_8x8_bytes` | 3.2451 µs | 3.2437–3.2465 µs | 415.66 ns | 415.50–415.84 ns |

The strict comparison concluded:

- `read_exact_64_bytes`: +0.3127%, classified as noise.
- `read_many_single_64_bytes`: -3.8153% (`p = 0`).
- `read_many_contiguous_8x8_bytes`: -1.0331% (`p = 0.32`), no change.
- `read_many_gapped_8x8_bytes`: -87.192% (`p = 0`), with +680.79% throughput.

These are non-portable, machine-local observations from one virtualized guest,
not a production performance promise.

### Autoresearch follow-up

The separate autoresearch run used:

```sh
bash tools/autoresearch/windows-memory-bench.sh
```

Methodology: 31 samples × 16,384 operations, reporting the median in ns/op
for each workload on the same Windows 11 QEMU/KVM guest with Rust 1.96.1.
The harness baseline and final canonical autoresearch medians were:

| Workload | Harness baseline median | Final autoresearch median | Change |
| --- | ---: | ---: | ---: |
| `gapped` | 412.52 ns/op | 395.94 ns/op | -4.0192% |
| `exact` | 374.82 ns/op | 372.36 ns/op | -0.6563% |
| `singleton` | 392.07 ns/op | 370.68 ns/op | -5.4557% |
| `contiguous` | 408.50 ns/op | 392.90 ns/op | -3.8188% |
| `wide same-page 8×8 bytes (3,368-byte span)` | 2,124.76 ns/op | 940.64 ns/op | -55.7319% |

These are non-portable, machine-local observations from one virtualized guest;
they are subject to benchmark noise and are not a production performance
promise.

The durable Criterion case for the wide layout was measured separately with:

```powershell
cargo bench --locked --offline --target x86_64-pc-windows-msvc --bench hot_paths -- windows_process_memory/read_many_wide_8x8_bytes
```

On the same Windows 11 QEMU/KVM guest and Rust 1.96.1 toolchain, Criterion 0.7.0
used its default 3-second warm-up and 100 samples. The post-change arithmetic
mean was 1,004.81 ns/iteration with a 95% confidence interval of
983.30–1,033.86 ns/iteration. This is a non-portable, machine-local observation,
not a production performance promise.

### Allocation-free serializer follow-up

The formatter comparison used the same Windows guest and toolchain:

```powershell
cargo bench --locked --offline --target x86_64-pc-windows-msvc --bench hot_paths -- serializers_cold
```

Both revisions used Criterion 0.7.0 defaults, a 3-second warm-up, and 100
samples. The table reports arithmetic-mean point estimates from
`new/estimates.json`; the change is candidate versus baseline.

| Benchmark ID | Allocating baseline | Stack-writer candidate | Change |
| --- | ---: | ---: | ---: |
| `serializers_cold/v1` | 7,143.29 ns | 6,262.72 ns | -12.3272% |
| `serializers_cold/v2` | 12,907.53 ns | 11,131.86 ns | -13.7568% |
| `serializers_cold/v2_precise` | 5,388.56 ns | 3,458.17 ns | -35.8238% |
| `serializers_cold/stream_companion` | 9,934.88 ns | 9,022.38 ns | -9.1848% |

Criterion classified `v1`, `v2`, and `stream_companion` as improved with
`p < 0.05`. The noisy `v2_precise` baseline produced a wide confidence interval,
so Criterion classified that row as no statistically significant change despite
the lower point estimate. Exact JSON-number boundary tests cover positive and
negative zero, integral floats, finite extrema, subnormal values, non-finite
values, and the ECMAScript fixed/exponent thresholds.

The Windows-only `mimalloc` candidate was rejected. One matched harness run
reduced process cycle counts, but aggregate CPU time moved in the opposite
direction and idle private bytes rose from approximately 3.9 MiB to
40,480,768 bytes. The system allocator remains in use.

These are non-portable, machine-local observations from one virtualized guest;
they are not production performance promises.

## Historical local observation: 2026-07-16

This section is a **non-portable observation, not a performance promise or
acceptance threshold**.

- Command: `cargo bench --bench hot_paths`
- Host: AMD Ryzen 9 9950X3D, 16 cores / 32 threads, 123 GiB RAM
- OS: x86_64 Linux 7.1.3-1-cachyos-bore
- Compiler: `rustc 1.96.1 (31fca3adb 2026-06-26)`
- Criterion: 0.7.0 defaults listed above; Plotters backend (gnuplot unavailable)
- Timestamp: 2026-07-16T11:02:30-04:00

The table reports Criterion's arithmetic-mean point estimate and 95% confidence
interval from the `new/estimates.json` files written by this run.

| Benchmark ID | Mean | 95% confidence interval |
| --- | ---: | ---: |
| `batch_scan/three_patterns_2mib` | 51.917 µs | 50.079–54.040 µs |
| `raw_snapshot_reads/stable_regular_fields` | 648.1 ns | 610.4–690.3 ns |
| `raw_snapshot_reads/lazer_regular_fields` | 103.803 µs | 102.889–104.793 µs |
| `map_calculation/parse` | 1.5467 µs | 1.5425–1.5508 µs |
| `map_calculation/full_difficulty_performance` | 12.056 µs | 11.996–12.122 µs |
| `map_calculation/gradual_all_objects` | 1.5807 µs | 1.5792–1.5823 µs |
| `map_calculation/unchanged_cache_key` | 582.4 ns | 568.3–599.6 ns |
| `serializers_cold/v1` | 4.571 µs | 4.541–4.603 µs |
| `serializers_cold/v2` | 9.664 µs | 8.586–11.756 µs |
| `serializers_cold/v2_precise` | 2.131 µs | 1.863–2.436 µs |
| `serializers_cold/stream_companion` | 8.664 µs | 7.441–10.235 µs |
| `serialization_cache/v1_bytes_clone` | 12.720 ns | 12.701–12.742 ns |
| `serialization_cache/v2_bytes_clone` | 13.260 ns | 13.144–13.389 ns |
| `serialization_cache/v2_precise_bytes_clone` | 12.822 ns | 12.775–12.870 ns |
| `serialization_cache/stream_companion_bytes_clone` | 13.220 ns | 13.117–13.336 ns |

The suite did not complete. At `publication/unfiltered_clients/1`, Criterion
warned that 100 samples could not complete in the 5-second target and estimated
`6000.2 s` for collection. The command was stopped after one hour while still
collecting that benchmark. Therefore this run produced no reportable current
measurements for `publication/unfiltered_clients/1`,
`publication/unfiltered_clients/100`, or `publication/unfiltered_clients/1000`;
older files for those IDs were deliberately excluded.

## 2026-09-05 audit: build weight and serialization

Baseline: `9fd6251d61d858c1ef851811b5bc41c69e505beb`, exported to an independent temporary directory. Candidate: the audit implementation working tree. Native Linux on the same Ryzen 9 9950X3D host, pinned Rust 1.96.1, offline dependency downloads already available. No Windows VM or native Windows timing was used.

Build and serializer figures below describe the measured candidate snapshot, before final report-cancellation lifetime and obsolete mod-fallback cleanup. The final Linux event-enabled executable is rebuilt and smoke-tested separately; its build time is not presented as another cold-build comparison.

### Cold release build observation

Separate empty target directories; `cargo build --release --locked --offline --no-default-features --timings -j 8` in each tree. These measurements deliberately omit process events in **both** builds to isolate the shared daemon dependency graph; production Linux still explicitly enables `linux-process-events` and its packaged smoke is exercised separately. Wall times include Cargo overhead and are single local observations, not CI-minute forecasts. The final Windows cross-check briefly overlapped the beginning of the candidate build, so this is not a controlled repeated build benchmark.

| Measurement | Baseline | Audit candidate |
| --- | ---: | ---: |
| Cold build wall time | 136.54 s | 106.06 s |
| Stripped binary size | 14,193,112 bytes | 11,703,336 bytes |

The lockfile removes 54 package/version entries from the report-only graphics dependency graph, adding two build-time asset-filter dependencies (`globset`, `bstr`) and the standalone smoke package without upgrading unrelated retained dependencies. Release smoke no longer links the daemon library into its runner.

A separate empty-target baseline build with `CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16` took 76.93 s and produced a 15,475,536-byte binary, versus 14,193,112 bytes with the existing single unit. Both executed `--help` successfully. The candidate was 9.0% larger and has no representative runtime validation; production therefore retains one codegen unit and thin LTO rather than trading runtime/size for a build-only result.

### Synthetic cold serializers

Run independently in both trees:

```sh
cargo bench --locked --offline --no-default-features --package vosu \
  --bench hot_paths -j 8 -- serializers_cold \
  --sample-size 20 --measurement-time 1 --warm-up-time 1
```

| Criterion ID | Baseline estimate [interval], µs | Candidate estimate [interval], µs |
| --- | ---: | ---: |
| `serializers_cold/v1` | 7.6863 [6.5743, 8.5364] | 6.9694 [6.1198, 7.5262] |
| `serializers_cold/v2` | 14.164 [13.413, 14.768] | 10.305 [10.134, 10.473] |
| `serializers_cold/v2_precise` | 5.6449 [4.9291, 6.2474] | 5.3594 [4.7815, 5.8700] |
| `serializers_cold/stream_companion` | 8.4245 [7.5452, 9.1648] | 8.1934 [7.4933, 8.7570] |

Only V2 separates clearly in this short run (27.2% lower central estimate). The other intervals overlap; no improvement claim is made for them. These are synthetic fixture results, not whole-daemon/live-gameplay measurements or a measurement of large replay retention. The populated-vector API regression checks byte equivalence between borrowed encoding and owned DTO serialization.

### Calculator microbenchmark check

The same compiled baseline and candidate benchmark executables also ran `map_calculation` with the 20-sample/1-second settings above. The fixture contains five objects and does **not** measure full-to-progressive preparation reuse.

| Criterion ID | Baseline central estimate | Candidate central estimate |
| --- | ---: | ---: |
| `map_calculation/parse` | 1.7212 µs | 1.7424 µs |
| `map_calculation/full_difficulty_performance` | 12.358 µs | 12.942 µs |
| `map_calculation/gradual_all_objects` | 10.713 µs | 10.424 µs |
| `map_calculation/unchanged_cache_key` | 10.429 ns | 10.687 ns |

The apparent full-calculation slowdown did not survive a reversed-order rerun of that exact filter with `--sample-size 50 --measurement-time 3 --warm-up-time 2`: candidate 12.787 µs [12.699, 12.871], then baseline 12.991 µs [12.899, 13.093]. This demonstrates ordering/run variation, not a reliable calculator speedup or regression. Do not use Criterion's automatic “change” labels across these direct invocations: both wrote the same local results directory. The explicit baseline/candidate estimates above are the comparison evidence. Existing calculator compatibility and state-transition tests remain the numerical-behavior gate.

## Five-lane autoresearch consolidation (2026-09-08)

The integration preserves the original histories from cleanup snapshot
`e0bb23e4d39729ec82a9f2bc5e75832a87ff7f86`; no rejected experiment is restored.
The retained lane tips are process `ce5b674`, decoder `8e8c7a7`, API
`7eb265b`, publication `a05da50`, and calculation `faeead2`.
These observations come from the preserved lane experiments, not a new
integrated speed comparison. Percentages across lanes are not additive.

| Lane | Retained observation | Limitation |
| --- | --- | --- |
| Process | Direct iovec construction removes intermediate allocations; scanner slot reuse removes 127 allocations/8,128 requested bytes in the 128-region probe; 4 KiB proc-maps capacity removes seven allocations/4,064 requested bytes per read. | Direct-read boundary-error medians were 0.91–1.38% slower; scanner benefits are established only for fragmented 128-region inputs. |
| Decoder | Lazer precise fixture backend calls 29 → 28, scatter batches 3 → 2, still 570 requested bytes; stable idle slice-backed snapshot median 606.28 → 235.525 ns. | No established lazer latency win; stable numbers measure decoding/slice copies, not native Windows memory reads. |
| API | v1 reserves 2 KiB, saving eight reallocations/2,040 requested bytes per cold encode; canonical index classification and compact in-place strain sorting retain wire order/capacity. | No v1 latency claim; empty-input latency varies. |
| Publication | Shared-plan/100-client ownership change improves 3.87–7.22% across three pairs; comparison caching helps large shared selected arrays. | Hub/library work, not network latency; scalar/distinct effects remain unresolved, and an inflated 1,000-client baseline is not a claimed gain. |
| Calculation | Immutable original/base maps are shared; difficulty-only preparation avoids unused state. Production `performance_from_score` synthetic-prefix probes improve 34.33–77.63% across four native-ruleset maps. | Maps contain only 93–238 top-level objects; no broad user-map or Windows claim. |

### Sparse-large tradeoff

Commit `ce5b674` excludes already-skipped empty requests from heap iovec
capacity without a second counting pass. Retain it for the reproduced
sparse heap-spill benefit and reduced reservation, accepting the measured
control regressions explicitly rather than calling it universally faster:

| 65,536 total requests | CPU 10 paired median | CPU 2 paired median |
| --- | ---: | ---: |
| 33 active, spread | 13.54% faster | 11.41% faster |
| 33 active, back | 12.79% faster | 10.64% faster |
| 32 active, spread (stack control) | 2.64% slower | 3.83% slower |
| All empty | 3.04% slower | 3.01% slower |

Both spread/back improvements won all eight pairs per CPU. Front-loaded
33-active requests were 1.45%/0.55% slower; dense controls were near flat,
and 4,096-request sparse inputs did not benefit. Unknown trailing empty
entries can still over-reserve. No production workload frequency was measured,
so a weighted whole-daemon gain cannot be inferred. Requested allocation
bytes are not RSS.

### Reproduction and retained evidence

The Linux host used Rust 1.96.1, kernel `7.2.2-1-cachyos-bore`, alternating
baseline/candidate pairs, CPU affinity, and a shared timing lock. These
controls do not eliminate shared-host load. Sparse-large used CPUs 10 and 2,
eight pairs/workload/CPU, 16 warmups, and an iteration multiplier of four.
Instrumentation was excluded from timed runs; 105 native correctness cases
per variant covered canaries, exact bytes, short reads, faults and overflow.

Machine-local records remain under `/home/user/.cache/`:

- `vosu-autoresearch-process-memory-20260907/evidence.jsonl` and
  `sparse-large/{evidence.jsonl,raw-pairs.csv,reproducer-archive.json}` within that directory.
- `vosu-autoresearch-memory-decoders-20260907/final-summary.json`.
- `vosu-autoresearch-api-serialization-20260907/continuation-final.json`.
- `vosu-autoresearch/publication-20260907/lane-summary.json`.
- `vosu-autoresearch-calculation-20260907/continuation.json`.

The records link exact build/run commands, raw samples, retained/rejected
decisions, and archived probes. Integration commands, exact HEAD, evidence
hashes, commit manifest and platform gaps are recorded separately in
`vosu-consolidation-20260908/result.json`. Native Windows and interactive
live-client parity require separate authorized verification; Linux synthetic
and daemon smoke results do not replace those gates.

## Final reopened-lane consolidation (2026-09-08)

This is a final combined measurement, not the sum of the earlier lane results.
The preceding section preserves the first integration's historical evidence.
All five reopened histories were merged without cherry-pick duplication:
process `50cbca3`, decoder `d559d11`, API `4afa60d`, publication `3f4ca2f`,
and calculation `e28e9c4`. No retained lane commit was excluded.

### Source identity and timing protocol

| Label | Actual archived source revision |
| --- | --- |
| Original optimization base | `e0bb23e4d39729ec82a9f2bc5e75832a87ff7f86` |
| Previous integration | `ce286f4fe8ea51a92684f8e7446bc10fab18ad9a` |
| Final benchmark source | `1ab55ae30fa5c9622a17f71deea816e076c37dec` |

Each revision was exported with `git archive`. All 889 exported tracked blobs
per archive matched that revision's Git blob IDs; per-file SHA-256 hashes are
retained. Build metadata distinguishes the coordinator HEAD from the actual
archive source revision. The final documentation commit does not change the
benchmarked Rust source.

All three archives used Rust 1.96.1, the same lockfile, host target, release
profile, and `--no-default-features --features linux-process-events`.
Build command: `cargo build --release --locked --no-default-features --features
linux-process-events --lib --bins --benches`. Standalone probes linked the
corresponding release libraries with `-C opt-level=3 -C lto=thin
-C codegen-units=1 -C panic=abort -C strip=symbols`; exact commands and binary
hashes are preserved, rather than inferring artifact identity from filenames.

The fixed matrix ran all six permutations of original/previous/final once
per workload: six observations per version, 40 workload groups, 720 successful
process invocations. Successful `taskset` launches pinned single-thread
workloads to CPU 2 and the four-worker publication probe to CPUs 8–11.
The shared timing `flock` covered the entire matrix. Builds, Clippy, tests,
and correctness probes finished before timing; accepted timing began 233.22
seconds after the last validation. Recorded intervals do not overlap.
One-minute host load ranged from 1.625 to 4.170. Affinity and the lock do not
make the shared host unloaded or control unrelated users' work.

Criterion used `--sample-size 20 --warm-up-time 0.2 --measurement-time 0.5
--nresamples 1000`. Every repetition's `sample.json`, `benchmark.json`, and
`estimates.json` was copied before the next repetition: 450 sample files,
9,000 raw observations. Other probes retain raw stdout and all six external
observations; API stdout also contains its internal encode samples and
publication stdout its per-round samples. Decoder stdout exposes internal
median summaries, not individual internal timings. No outlier was deleted.
An initial six-invocation attempt omitted Criterion raw-sample copies; the
entire attempt is explicitly excluded and preserved separately. The complete
matrix above was rerun with corrected capture.

### Combined measurements

Times below are microseconds per operation, except that the live-prefix
calculator probe elsewhere in the full table measures a complete sequence
of score updates. Changes are medians of six within-round percentage changes,
not ratios of the displayed medians. Negative means less time. Final CV is
the sample standard deviation divided by the mean of six external values.
The machine-readable table includes every metric, raw external values,
variance, range, both comparisons, and faster-pair counts.

| Workload | Original µs | Previous µs | Final µs | Δ original | Δ previous | Final CV |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Production backend, osu! | 2245.00 | 2036.01 | 173.74 | -92.26% | -91.46% | 0.35% |
| Production backend, taiko | 3758.48 | 3567.94 | 292.83 | -92.22% | -91.79% | 0.31% |
| Production backend, catch | 238.71 | 171.01 | 29.32 | -87.71% | -82.84% | 0.77% |
| Production backend, mania | 523.93 | 484.29 | 55.75 | -89.36% | -88.49% | 0.81% |
| Mania parse control | 20.83 | 20.19 | 21.19 | +1.64% | +4.89% | 1.25% |
| Native lazer precise capture | 8.180 | 7.978 | 6.719 | -18.51% | -16.04% | 1.24% |
| Stable slice-backed idle | 0.611 | 0.245 | 0.238 | -60.99% | -1.81% | 1.74% |
| Stable key-overlay control | 0.237 | 0.237 | 0.236 | -0.09% | -0.61% | 1.20% |
| V1 stable idle, eight orphan spectators | 2.773 | 2.598 | 2.389 | -13.79% | -8.26% | 0.43% |
| StreamCompanion stable idle, 2,048 strains | 363.43 | 320.70 | 318.90 | -12.55% | -0.48% | 0.33% |
| V2 stable idle, 2,048 graph points | 237.99 | 245.18 | 242.34 | +2.25% | -1.16% | 1.29% |
| Sparse 65,536 requests, all empty | 13.755 | 13.784 | 13.806 | +0.37% | +0.16% | 0.31% |
| Sparse 65,536 requests, 32 spread | 16.410 | 16.443 | 16.397 | +0.04% | -0.17% | 0.27% |
| Sparse 65,536 requests, 33 spread | 16.440 | 16.479 | 16.490 | +0.24% | -0.11% | 0.33% |
| Sparse 65,536 requests, 33 back | 15.501 | 15.941 | 15.873 | +2.52% | -0.47% | 0.29% |
| Sparse 65,536 requests, 33 front | 15.925 | 18.615 | 18.560 | +16.48% | -0.10% | 0.95% |
| Sparse 65,536 requests, 64 spread | 18.616 | 21.227 | 21.211 | +13.69% | -0.96% | 1.36% |
| Fresh regions, 128 fragments | 26.918 | 26.175 | 25.891 | -3.85% | -1.10% | 0.23% |
| Cold regions, 128 fragments | 27.178 | 26.479 | 26.584 | -2.05% | +0.44% | 0.34% |
| Boundary-error reads, 128 fragments | 34.953 | 33.888 | 32.136 | -8.04% | -5.24% | 0.15% |
| Four-worker subscriptions, 1,000 shared, fast | 349.68 | 340.02 | 206.39 | -40.97% | -39.70% | 3.24% |
| Four-worker subscriptions, 1,000 distinct, fast | 430.64 | 404.00 | 418.57 | -2.13% | +4.03% | 2.29% |
| Four-worker subscriptions, 1,000 distinct, slow | 404.71 | 412.49 | 410.19 | +2.35% | -0.35% | 2.54% |
| Four-worker subscriptions, 1,000 shared, slow, 1,024 integers | 2165.31 | 318.02 | 197.71 | -90.88% | -37.63% | 4.11% |

The production backend rows exercise `LiveBeatmapEngine::calculate`, including
attributes, strains, and generated accuracy scores. They use one existing
native-ruleset `*-diffcalc-test.osu` per mode, not a representative user-map
distribution. Live-prefix sequences improved 50.60–84.58% against original
and 24.32–48.83% against previous. In the archived binaries and measurement
protocol, native map parsing was 1.60–4.89% slower than previous across all
four maps; gradual controls ranged from -0.04% to +0.27%. These unfavorable
observations remain retained, not universal calculator wins or evidence of
an intrinsic parser slowdown.

The subsequent read-only parser diagnosis found unchanged previous/final
allocation, reallocation, and requested-byte counts on all four maps.
Retired-instruction differences in the exact archived binaries were negligible
(-0.065% to +0.038% per added parse), not enough added instruction work to
explain the timing gap. Parsing and construction source remained unchanged;
the preparation and live-prefix optimizations do not run inside the timed
parse branch.

A bounded disposable-harness diagnostic compared setup present versus absent
using the same retained previous/final libraries, identical 64-parse warmup
before optional setup, six balanced orders, and grouped hardware counters.
Final's median paired time changed sign relative to the archived regression
in **both** controls: setup absent measured -5.99%, -5.32%, -4.83%, and -2.29%
for osu!, taiko, catch, and mania; setup present measured -2.63%, -6.16%,
-3.55%, and -5.01%. Removing setup therefore did not consistently remove a
final-version penalty, and cannot be credited with fixing the original result.
The setup-absent path added about two instructions per parse in final;
generic cache-miss counts were small and noisy, not a causal explanation.

The exact cause remains unresolved. Relinking/code placement, the added
common warmup, allocator state, and execution conditions differ between this
diagnostic and the archived harness. Keep the original numbers as
**archived-binary/protocol-specific observations**; neither the sign reversal
nor unchanged allocation counts establish a parser speedup or fix.
[Parser diagnosis and setup-intervention evidence](file:///home/user/.cache/vosu-parser-diagnosis-20260908/result.json)
is retained locally on the measurement host, not a published artifact.

Across all 24 inputs per API group, ordinary V1 changes against previous
ranged -2.72% to +0.89%, StreamCompanion -1.45% to +2.16%, and the tournament
manager control -1.35% to +2.43%. V1 orphan-spectator inputs improved
2.67–9.38% against previous. Some ordinary V1 observations had 24.39% CV.
Small timing differences on unchanged paths are not attributed to source
improvements. In particular, V2 contains no newly retained serializer change.

The original sparse spread/back lane result is not reproduced as an
original-base improvement in this combined run. Front and 64-spread cases
are materially slower than original; the reopened process delta adds little
change relative to previous on these cases. All histories remain included,
with this cost explicit. No weighted real-daemon benefit is established.

Publication results are in-memory hub measurements, not network latency.
Shared plans improve, but the distinct-plan fast-writer regression remains.
The large original shared-array baseline is workload-specific, not a blanket
90% publication gain. Single-thread Criterion subscription and calculator
groups had roughly 22–27% final CV; their small control deltas, including the
unchanged cache key (+0.34% versus previous), are inconclusive. Their complete
results and raw samples remain in the evidence.

### Allocation, correctness, and evidence

The native decoder requested 570 / 570 / 912 bytes per cycle for original /
previous / final, with 29 / 28 / 26 backend calls. Fewer calls trade for 60%
more requested bytes. Stable idle/key probes use slice-backed memory; their
timings are not a Windows process-read speed claim.

Cold V1 idle requested 4,435 / 2,395 / 2,395 allocation bytes, with 36 / 28 / 28
reallocations. Eight orphan spectators requested 7,895 / 5,855 / 2,491 bytes
and 70 / 70 / 61 allocations; the manager control retained previous counts.
StreamCompanion's 2,048-strain case requested 994,931 / 852,122 / 852,122
bytes with 6,303 / 2,206 / 2,206 allocations. Cache hits allocated nothing in
the measured API cases. These are allocator-requested bytes, not RSS.

Linux workspace tests, Clippy, format, native process tests, and actual
release-daemon smoke passed at the benchmark source revision. Windows MSVC
cross-Clippy passed with `C_INCLUDE_PATH` unset. Cross-version probes matched
288 API documents, 60 calculator proof cases, and four production backend
outputs exactly. Native Windows execution, live-client parity, and a newly
packaged archive smoke remain unrun; no VM or game was accessed.

Evidence root: `/home/user/.cache/vosu-consolidation-final-20260908/`.
`result.json` links the exact commands, heads, checks, source-size audit,
included-commit manifest, and final documentation commit.
`archive-provenance.json`, `benchmark-artifacts.json`, and
`harness-manifest.json` identify sources and binaries.
`measurement-plan.json`, `measure.py`, and `analyze.py` reproduce the fixed
protocol; `timing.jsonl` and `timing/` retain accepted outputs and raw samples.
`benchmark-summary.json`, `benchmark-table.csv`, and `allocation-table.json`
contain the full results. `measurement-integrity.json` records validation
separation, source-attribution correction, and the excluded incomplete
raw-capture attempt. Earlier lane and first-integration evidence is untouched.
