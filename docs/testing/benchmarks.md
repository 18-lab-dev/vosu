---
type: Benchmark Guide
title: vosu benchmarks
description: Current local benchmark results, commands, and fixture limitations.
tags: [testing, benchmarks, criterion, performance]
---

# Benchmarks

## Reproduce

Run from the repository root with the pinned Rust toolchain. Bench profiles
use release-mode embedding: prepare the matching helper bundle first with
Python and the .NET 8 SDK (maintainer build dependencies, not end-user requirements):

```sh
python scripts/prepare-offset-generator.py --rid linux-x64
export CRITERION_HOME="$(mktemp -d)"
cargo bench --locked --package vosu --bench hot_paths
cargo bench --locked --no-default-features --bench memory_reader_bench
```

Implementations: `benches/`; targets: `Cargo.toml`. Append `-- <group-or-ID>`
to filter Criterion cases.

## Upstream 2359305 update comparison

Measured on **2026-09-29**: the working tree targeting upstream
`2359305a63f2aaebb201bc8d2eee58177f52590e` versus vosu's pre-update
`9c2187e683775485b8551fd6e7cb09b6e6786332`. Ryzen 9 9950X3D,
Linux `7.2.6-1-cachyos-bore`, Rust 1.96.1, performance governor, no CPU
pinning/isolation. Sampled one-minute host load was 2.78 during the updated
run and 2.48 after the baseline run.

Both used locked dependencies, release defaults, 100 samples, 3 s warm-up and
5 s measurement. The lockfiles were identical. The benchmark source differed
only by the required `room: Default::default()` snapshot initializer; new wire
fields are part of the updated serializer workload. The recorded baseline
was rebuilt in an independent target directory from a `git archive` of the
pre-update revision; an earlier shared-target trial is excluded.

Run from each checkout with its own `CARGO_TARGET_DIR` and `CRITERION_HOME`:

```sh
cargo bench --locked --package vosu --bench hot_paths -- '^(raw_snapshot_reads|map_calculation|serializers_cold)/'
```

All ten cases completed. The updated run's initial invocation hit the runner
timeout after compilation and three completed cases; the remaining seven
were measured separately with the same settings. Values below are arithmetic
means from `new/estimates.json`, not Criterion's console slope estimates.
Positive change means slower; percentage changes are descriptive, not
independent statistical significance claims.

| Benchmark ID | Pre-update mean | Updated mean | Updated 95% confidence interval | Mean change |
| --- | ---: | ---: | ---: | ---: |
| `raw_snapshot_reads/stable_regular_fields` | 201.400 ns | 232.155 ns | 231.561–232.845 ns | +15.3% |
| `raw_snapshot_reads/lazer_regular_fields` | 84.221 µs | 5.414 µs | 5.313–5.513 µs | -93.6% |
| `map_calculation/parse` | 1.587 µs | 1.568 µs | 1.562–1.575 µs | -1.2% |
| `map_calculation/full_difficulty_performance` | 12.003 µs | 11.788 µs | 11.732–11.873 µs | -1.8% |
| `map_calculation/gradual_all_objects` | 10.158 µs | 10.102 µs | 10.078–10.132 µs | -0.6% |
| `map_calculation/unchanged_cache_key` | 10.145 ns | 10.060 ns | 10.045–10.079 ns | -0.8% |
| `serializers_cold/v1` | 4.543 µs | 4.337 µs | 4.269–4.412 µs | -4.5% |
| `serializers_cold/v2` | 10.092 µs | 9.747 µs | 9.386–10.409 µs | -3.4% |
| `serializers_cold/v2_precise` | 3.479 µs | 3.429 µs | 3.364–3.498 µs | -1.5% |
| `serializers_cold/stream_companion` | 6.001 µs | 6.168 µs | 5.646–7.156 µs | +2.8% |

The stable zero-filled-fixture path is about 31 ns slower. The large lazer
difference must not be presented as a live-reader speedup: this fixture
discards results from zero-filled memory, while the offset layout and reader
behavior changed. Cold serializer estimates contain outliers, especially
StreamCompanion. These observations do not measure populated multiplayer
rooms, incremental UR, a real client, or whole-daemon CPU use.


## Security generation checks — 2026-10-02

Measured the patched, uncommitted tree based on `d9bdfb4e90fa165c809bced20e699e5a71a82d0f` on the local Linux `7.2.6-1-cachyos-bore` host with pinned Rust 1.96.1:

```sh
cargo bench --locked --package vosu --bench hot_paths -- linux_process_memory/current_process_64_bytes --warm-up-time 1 --measurement-time 3 --sample-size 20
```

Criterion reported **300.45 ns** per 64-byte self-process read, with interval **300.10–300.92 ns**, and **203.14 MiB/s**. The run completed all 20 samples. Generation checks surround the native read; the default-feature path uses the retained pidfd where available rather than reading `/proc/<pid>/stat` for each syscall.

Criterion also reported a **79.7% time increase** against its retained local baseline. That baseline's exact source/machine-state provenance was not re-attested for this run; this is not a controlled before/after comparison or a whole-daemon CPU claim. The benchmark does not measure a live game, multiplayer decoding, or the no-pidfd stat fallback. Retain the earlier results below as dated evidence rather than rewriting them.

## Prior full-suite local results

- **2026-09-09**, revision `9b0648c4b90d5b9eeddd480e1c7029c7f8be71d2`
  with existing uncommitted changes; source hashes stayed unchanged during the run.
- Ryzen 9 9950X3D, CachyOS Linux `7.2.2-1-cachyos-bore`, Rust 1.96.1.
  Performance governor, no CPU pinning or isolation; host load averaged 8.21 → 5.00.
  Results describe this working tree and machine, not a clean-commit baseline.

### Hot paths

All 25 Linux cases completed successfully. Arithmetic means and 95% confidence
intervals from Criterion's `new/estimates.json`; defaults: 100 samples,
3 s warm-up, 5 s measurement. Wide intervals require isolated reruns.

| Benchmark ID | Mean / iteration | 95% confidence interval |
| --- | ---: | ---: |
| `batch_scan/three_patterns_2mib` | 48.736 µs | 48.311–49.165 µs |
| `linux_process_memory/current_process_64_bytes` | 168.792 ns | 168.063–169.604 ns |
| `map_calculation/full_difficulty_performance` | 12.699 µs | 12.637–12.766 µs |
| `map_calculation/gradual_all_objects` | 10.448 µs | 10.390–10.517 µs |
| `map_calculation/parse` | 1.646 µs | 1.640–1.652 µs |
| `map_calculation/unchanged_cache_key` | 11.079 ns | 10.588–11.672 ns |
| `publication/unfiltered_clients/1` | 2.070 ms | 2.066–2.075 ms |
| `publication/unfiltered_clients/100` | 2.084 ms | 2.077–2.091 ms |
| `publication/unfiltered_clients/1000` | 2.151 ms | 2.136–2.165 ms |
| `raw_snapshot_reads/lazer_regular_fields` | 85.332 µs | 84.918–85.792 µs |
| `raw_snapshot_reads/stable_regular_fields` | 572.740 ns | 571.088–574.651 ns |
| `serialization_cache/stream_companion_bytes_clone` | 13.004 ns | 12.950–13.063 ns |
| `serialization_cache/v1_bytes_clone` | 12.873 ns | 12.839–12.922 ns |
| `serialization_cache/v2_bytes_clone` | 13.741 ns | 13.558–13.929 ns |
| `serialization_cache/v2_precise_bytes_clone` | 13.838 ns | 13.525–14.312 ns |
| `serializers_cold/stream_companion` | 7.040 µs | 6.316–8.371 µs |
| `serializers_cold/v1` | 6.896 µs | 5.328–9.826 µs |
| `serializers_cold/v2` | 10.224 µs | 10.047–10.417 µs |
| `serializers_cold/v2_precise` | 4.588 µs | 4.389–4.789 µs |
| `v2_subscription/distinct_plans/1` | 8.414 µs | 8.288–8.595 µs |
| `v2_subscription/distinct_plans/100` | 56.661 µs | 56.154–57.234 µs |
| `v2_subscription/distinct_plans/1000` | 595.639 µs | 589.851–601.941 µs |
| `v2_subscription/shared_plan/1` | 8.132 µs | 8.102–8.167 µs |
| `v2_subscription/shared_plan/100` | 38.781 µs | 38.642–38.948 µs |
| `v2_subscription/shared_plan/1000` | 354.395 µs | 350.316–361.043 µs |

### Deterministic memory reader

One successful invocation; medians of nine sample means. Lazer: 2,000 warm-up
iterations, 1,000/sample. Stable: 20,000 warm-up iterations, 20,000/sample.

| Workload | Time / cycle |
| --- | ---: |
| Native self-process lazer precise capture, 18 keys | 8.315 µs |
| Stable idle decoder | 628.69 ns |
| Stable key-overlay decoder | 236.47 ns |

Counting fixtures: lazer 26 exact + 3 batch reads / 570 requested bytes per cycle;
stable key-overlay 5 exact + 2 batch reads / 258 bytes. These are API counts,
not syscall or allocation measurements.

## Native Windows coverage

**Hosted Criterion measurements remain pending.** The manual
[`Windows benchmarks`](../../.github/workflows/benchmarks-windows.yml) workflow
runs both targets on `windows-latest` with Rust 1.96.1 and native MSVC, including
all five `windows_process_memory` cases. It uploads logs, exit codes, revision,
runner details and Criterion samples as `windows-benchmarks-<commit>` (14 days).
Hosted timings are not directly comparable with the local Linux results.

Reproduce in PowerShell:

```powershell
python scripts/prepare-offset-generator.py --rid win-x64
cargo bench --locked --no-default-features --package vosu --target x86_64-pc-windows-msvc --bench hot_paths
cargo bench --locked --no-default-features --package vosu --target x86_64-pc-windows-msvc --bench memory_reader_bench
```

## Scope of the earlier synthetic results

- Synthetic fixtures, not live-game or whole-daemon performance. Raw snapshots
  discard reads from zero-filled fixtures; the deterministic reader validates
  populated graphs (native self-process lazer reads; slice-copy stable reads).
- Calculation uses a five-object map. Cold serializers exclude setup; cache
  cases clone encoded bytes. Publication includes timer scheduling; neither
  publication nor subscriptions measures network transport.
- Live-game, upstream, browser and privileged-event comparisons were not
  measured. No VM access or security changes were performed.

## Windows live precise cadence (2026-09-29)

Separate from the synthetic/hosted benchmarks above, an attested native Windows
stable capture on the local VM measured precise WebSocket delivery at **87.0 Hz
upstream and 90.0 Hz in vosu**, with zero gaps exceeding the fixed 450 ms stall
threshold. The active windows contained 2,089 and 2,166 observations,
respectively. The earlier retained stable capture measured 87.6 Hz upstream
and 60.0 Hz in vosu; these are separate runs, not a controlled throughput ratio.

The corrected daemon holds a balanced 1 ms Windows multimedia timer-resolution
request while each instance worker clock exists. Reproduce using the pinned
artifacts, 150 ms regular polling, 25 ms HTTP capture interval, and the complete
menu → play → pause → result → close procedure in the
[verification guide](verification.md#session-clock-and-cadence-remediation).
The evidence is `remediation-7c0j_0ys/clock/stable-minimum-skew-report.json`
under the retained verification directory named there. This capture still
fails one-to-one HTTP reference coverage; cadence measurements do not establish
full parity, timer deadline guarantees, or reduced CPU usage.

## Released tosu comparison (2026-10-09)

This campaign compares vosu **0.3.0** with the released tosu **4.26.2**
package/source at `b21b4526d8db9ce124f5a8bf475ef5421837f5b7`.
It does not compare against the former optimization base. Windows ran before
Linux; neither platform's measurements stand in for the other.

The measured vosu implementation is committed as
`230bde863b707c1ecac9b20bf9992290c20909e1`. The builds used
`f075408209ee98cdd780f3a4d386d2a9850f40b0` plus the client-runtime
dependency-closure fix. The measured production executables have SHA-256:

- Windows: `e8664104de180d98bfea29e437b37de61f78fc32b86a65c75c160ca4be2d304e`.
- Linux: `a8b20a661d1fb0281d8ddd0fd397c2367d3845f1d08b9747392ffb56b81ce07a`.

Retained local evidence is under
`~/.cache/vosu-final-20261008-4r3azwf2/`. Result files, configurations,
per-invocation logs, source/runtime manifests, and executable hashes distinguish
the accepted runs from rejected startup, workload, and stale-connection trials.
These are local observations, not hosted CI results or live-parity certification.

### Native Windows whole-daemon resources

Both production daemons read the same offline lazer **2026.726.0** client,
playing the six-minute original deterministic beatmap
`fa80da4663f37aaba8077be9ad459718` with Autoplay, osu! mode, and rate 1.
Regular/precise polling are 150/10 ms; PP calculation and key overlay are on.
Each mode has three simultaneous 45-second pairs, each after 30 seconds of
warm-up; API-worker submission order alternates vosu → tosu → vosu.
Background mode probes the API only at the window boundaries. Consumer mode
requests `/json/v2` at 10 Hz per daemon.

CPU is accumulated process-tree user/system time divided by elapsed wall
time, as a percentage of one logical core. Memory is sampled at 1 Hz.
The table reduces the three per-window medians; ranges span those medians.
MiB is bytes / 1,048,576; reductions use unrounded values.

| Mode / metric | Released tosu median (range) | vosu median (range) | vosu reduction |
| --- | ---: | ---: | ---: |
| Background CPU, one core | 2.708% (2.604–2.917) | 1.597% (1.493–1.701) | 41.02% |
| Background working set | 124.867 MiB (124.664–125.547) | 91.219 MiB (90.920–91.273) | 26.95% |
| Background private committed | 116.840 MiB (116.256–117.215) | 7.836 MiB (6.904–7.914) | 93.29% |
| 10 Hz consumer CPU, one core | 3.265% (3.229–3.368) | 1.667% (1.389–1.771) | 48.95% |
| 10 Hz consumer working set | 131.027 MiB (130.789–131.092) | 90.932 MiB (90.855–91.223) | 30.60% |
| 10 Hz consumer private committed | 123.535 MiB (122.484–123.539) | 7.090 MiB (7.062–7.926) | 94.26% |

All six pairs passed executable/PID/start-time, listener ownership,
client/build/map/modifier, play-clock progression, paired-clock skew, and API
admission checks. There were no API errors in the accepted files
`windows-play-background-fresh-sockets.json` and
`windows-play-consumer-final.json`; their matching `*-config.json` files
retain the workload identities. One-second tree sampling can miss transient
children. Windows private committed bytes are **not** Linux private resident
memory, and these resource observations do not establish schema parity.

### Linux VM whole-daemon resources

Both production daemons read the same offline lazer **2026.804.2** client
with the same six-minute beatmap checksum, osu! mode, Autoplay, and rate 1.
Polling and calculation settings match the Windows campaign; the client build
does not, so compare products within a platform, not timings between platforms.
Background and 10 Hz `/json/v2` consumer modes each have three simultaneous
45-second pairs after 30-second warm-ups, with the same alternating API-worker
submission order.

CPU is process-tree user/system time divided by wall time, as a percentage of
one logical core. Each window contains 46 one-second resource observations.
Memory is the arithmetic mean within each window; the table reports the median
and range across the three window means. CPU follows the same three-window
reduction. MiB is bytes / 1,048,576; percentage reductions use unrounded values.

| Mode / metric | Released tosu median (range) | vosu median (range) | vosu reduction |
| --- | ---: | ---: | ---: |
| Background CPU, one core | 1.535% (1.490–1.580) | 1.068% (1.067–1.090) | 30.41% |
| Background RSS | 244.091 MiB (243.129–249.316) | 85.820 MiB (85.820–85.879) | 64.84% |
| Background private resident | 234.353 MiB (233.390–239.578) | 82.137 MiB (82.137–82.195) | 64.95% |
| 10 Hz consumer CPU, one core | 2.091% (2.024–2.091) | 1.179% (1.135–1.246) | 43.63% |
| 10 Hz consumer RSS | 257.006 MiB (253.348–257.357) | 86.032 MiB (86.023–86.063) | 66.53% |
| 10 Hz consumer private resident | 247.268 MiB (243.609–247.618) | 82.348 MiB (82.340–82.380) | 66.70% |

All six pairs passed the executable/PID/start-time, listener ownership,
client/build/map/modifier, play-clock progression, paired-clock skew, and API
checks. Background windows contain two API probes per product; consumer
windows contain 451 requests per product. All accepted requests succeeded.
CPU deltas, memory means, identity stability, workload fields, and paired
clock skew were independently recomputed from the retained observations.

The task-owned process ancestry is vosu → tosu → lazer, enabling ordinary
same-UID reads under Yama `ptrace_scope=1` without capabilities or policy
changes. Resource accounting excludes tosu's identity-verified subtree from
vosu and the game's identity-verified subtree from tosu. The launcher/client
are not charged to either daemon. vosu selected **CN_PROC** after its
unprivileged eBPF probe failed.

RSS sums each charged process's `smaps_rollup` RSS; shared pages can be
double-counted. Private resident memory sums `Private_Clean`, `Private_Dirty`,
and `Private_Hugetlb`, not Windows private committed bytes. One-second tree
sampling can miss transient children, and kernel tick quantization can
dominate short CPU windows. API admission is not live schema parity.

Evidence is in `linux-vm-resources/`: `linux-vm-play-background.json`,
`linux-vm-play-consumer.json`, shared `resources-config.json`, platform/input
`resources-metadata.json`, the launcher, settings, and daemon logs. The guest
has eight vCPUs, kernel **7.1.4-zen1-1-zen**, glibc **2.43**, Python **3.14.6**,
and psutil **7.2.2**. The game uses software OpenGL on a task-owned
labwc/Xwayland desktop/profile; no guest governor files were exposed.
Campaign-start load averages were **3.662 / 3.388 / 2.107**. No CPU isolation,
privilege elevation, or system-policy changes were used. Host-Linux gameplay
attempts are excluded.

An excluded guest `--version` probe started the daemon and ran its ordinary
legacy-settings migration in the default guest configuration directory. That
probe was stopped; accepted campaigns used task-owned profiles and configuration
directories. The migrated settings were retained rather than reconstructing
unrecorded legacy-file bytes.

### Precise-reader microbenchmarks

The autoresearch-style protocol uses seven alternating-order invocation pairs,
2,000 warm-up cycles, then nine samples of 1,000 cycles per invocation.
Each invocation reports the median sample mean; the table reports the median
and range across the seven invocations. All 28 invocations exited successfully.

The timed fixture is a resolved lazer precise state/clock, 18 keys, and an
empty hit-error list in 1 MiB of synthetic memory. Setup is excluded. The
released reader requires direct initialization of its private resolved screen
and game-base pointers; it has no `synchronize_precise` method. Its key-overlay
method has no mode argument. Neither adaptation replaces its production
decoding or native process-read methods.

| Platform | Released tosu ns/cycle, median (range) | vosu ns/cycle, median (range) | tosu / vosu latency |
| --- | ---: | ---: | ---: |
| Native Windows | 74,553.70 (73,425.90–77,495.40) | 19,571.50 (19,375.40–21,263.80) | 3.810 |
| Linux VM | 31,442.79 (30,537.31–32,128.88) | 109,665.60 (105,448.30–111,623.12) | 0.287 |

**The Linux VM precise-reader result is unfavorable:** vosu took 3.49 times as
long in this fixture. Do not infer a cross-platform reader speedup from the
Windows result or generalize either fixture timing to whole-daemon CPU.

Tosu reports 156 typed native reads and 1,020 requested bytes per cycle;
vosu reports 24 exact plus two batched calls and 912 requested bytes.
Tosu counts wrapped production typed methods during timing; vosu's accounting
uses a separate counting reader, while its timed path reads native self-process
memory. Counts are instrumented API calls, not syscall/allocation measurements.
Tosu asserts each key's values and the empty error array; the retained vosu
benchmark asserts readiness, time, key count, and error index, not all of
those same values. Fixture intent is aligned, but assertion and instrumentation
coverage differ.

Stable idle/key-overlay timings also appear in the vosu output. They use
slice-memory decoder fixtures and are excluded from this native precise-reader
comparison.

Windows evidence: `readers-windows-closure/results.json` and seven numbered
log pairs. Linux VM evidence: `readers-linux-vm/manifest.json` and its 14
stdout/stderr logs. The VM manifest retains Node **24.14.0**, Rust **1.96.1**,
kernel **7.1.4-zen1-1-zen**, QEMU Q35 identification, eight vCPUs, glibc
**2.43**, and campaign-start load averages **0.062 / 0.015 / 0.004**.
No guest CPU-governor files were exposed. No CPU isolation, privilege
elevation, or system-policy changes were used. Host-Linux measurements in
`readers-linux-final/` and rejected host gameplay attempts are excluded.
