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
bash autoresearch.sh
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
