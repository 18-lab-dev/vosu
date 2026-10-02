---
type: Benchmark Guide
title: vosu benchmarks
description: Current local benchmark results, commands, and fixture limitations.
tags: [testing, benchmarks, criterion, performance]
---

# Benchmarks

## Reproduce

Run from the repository root with the pinned Rust toolchain:

```sh
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
cargo bench --locked --no-default-features --package vosu --target x86_64-pc-windows-msvc --bench hot_paths
cargo bench --locked --no-default-features --package vosu --target x86_64-pc-windows-msvc --bench memory_reader_bench
```

## Scope

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
