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

- `SliceMemory` reads from an `Arc<[u8]>` and copies slices in the benchmark
  process. `read_many` is a loop over those copies and `readable_regions`
  returns one synthetic region. **The memory benchmarks do not measure OS
  process-memory reads**, syscalls, cross-process access, page faults,
  permissions, or a live osu! process.
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
