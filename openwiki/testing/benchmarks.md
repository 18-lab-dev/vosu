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
| `windows_process_memory` | `read_exact_64_bytes`, `read_many_single_64_bytes`, `read_many_contiguous_8x8_bytes`, `read_many_gapped_8x8_bytes` | Windows-only current-process `ReadProcessMemory` calls through the production `ProcessMemory` reader. `read_many` combines ordered non-overlapping requests only when their full span fits a fixed 1024-byte stack scratch and one 4 KiB page; singleton requests bypass the batching scratch. Throughput is declared as 64 bytes per iteration. |
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
- The Windows-only `windows_process_memory` cases call production `ProcessMemory` against the current benchmark process through native `ReadProcessMemory`: `read_exact_64_bytes` is one exact 64-byte request, while `read_many` covers singleton, contiguous, and gapped ordered non-overlapping requests in a synthetic `#[repr(align(4096))]` page. They measure current-process native syscalls and synthetic aligned same-page layouts, not cross-process faults or a live osu! process. Production `read_many` bypasses batching scratch for singleton requests; otherwise it uses a fixed 1024-byte stack scratch and merges across gaps only when the full span fits that capacity and one 4 KiB page, retries unread suffixes after short reads, rejects a false backend result even when the byte count equals expected, and zeroes all destinations if any read fails.
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

These are non-portable, machine-local observations from one virtualized guest;
they are subject to benchmark noise and are not a production performance
promise.

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
