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

## Current local results

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

**Pending first hosted run; no Windows measurements claimed.** The manual
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
