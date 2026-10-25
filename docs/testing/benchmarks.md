---
type: Benchmark Guide
title: vosu benchmarks
description: Latest local benchmark results, commands, and fixture limitations.
tags: [testing, benchmarks, criterion, performance]
---

# Benchmarks

This guide keeps only the latest result of each benchmark. A rerun replaces
its section; earlier results stay in this file's Git history
(`git log -p -- docs/testing/benchmarks.md`).

## Reproduce

Run from the repository root with the pinned Rust toolchain. Bench profiles
use release-mode embedding: prepare the matching helper bundle first with
Python and the .NET 8 SDK (maintainer build dependencies, not end-user requirements):

```sh
python scripts/prepare-offset-generator.py --rid linux-x64
export CRITERION_HOME="$(mktemp -d)"
cargo bench --locked --package vosu --bench hot_paths
cargo bench --locked --bench memory_reader_bench
```

Implementations: `benches/`; targets: `Cargo.toml`. Append `-- <group-or-ID>`
to filter Criterion cases.

Keep the default `linux-process-events` feature for the Linux memory reader;
releases ship it. With `--no-default-features` the native reader has no pidfd
and checks the process generation by reading `/proc/<pid>/stat` after every
native read, so the timing measures that fallback instead. Current pidfd-backed
reads also check once, after the read. Older measurements below retain their
original source-generation semantics and are not measurements of this change.
Windows releases use no default features, so the Windows commands below keep
`--no-default-features`.

### Live native reader

The dev-only example attaches read-only to an existing client PID and measures
the production regular and precise capture adapters, without network publication.
Setup is excluded; nine sample means follow warm-up. Keep the client in the same
state throughout the run. Lazer uses the daemon's versioned cache or embedded
offsets; if unavailable, run the release daemon once to populate the reported
cache path. Read/byte counts are null because the native reader has no counters.

```sh
cargo build --release --locked --example live_reader_bench
target/release/examples/live_reader_bench --pid 1234 --client lazer --warmup 100 --cycles 1000 --samples 9
```

Current lazer regular captures reuse the beatmap head and track length read by
`global`. With a player, menu fields, beatmap hash, and MP3 length therefore
observe the global-read instant rather than independent later reads. Without
a player, the hash may still be read in `menu`. This is capture-local reuse,
not a cache across captures. The reader integration excludes `ConfigPlan`;
do not attribute changes to that separate configuration-plan experiment.

## Scope of the synthetic results

- Synthetic fixtures, not live-game or whole-daemon performance. Raw snapshots
  discard reads from zero-filled fixtures; the deterministic reader validates
  populated graphs (native self-process lazer reads; slice-copy stable reads).
- Calculation uses a five-object map. Cold serializers exclude setup; cache
  cases clone encoded bytes. Publication includes timer scheduling; neither
  publication nor subscriptions measures network transport.
- Live-client and released-tosu measurements are in the
  [released tosu comparison](#released-tosu-comparison).

## vosu hot paths

Current Linux host and native Windows guest results follow. Each platform
compares versions on the same machine; absolute timings across the platforms
are not comparable. Ranges span invocation means, not confidence intervals.

### Linux

- Measured **2026-10-25**, final 1.2.0 source `77080cd`, Ryzen 9 9950X3D
  **host**, kernel `7.2.10-1-cachyos-bore`, Rust 1.96.1, performance governor,
  boost enabled, default release features and locked dependencies; VM off.
- Three invocations per case, pinned to CPU 15 (frequency CCD), **taskset
  only, no CPU/IRQ isolation**. Values are medians and min–max ranges of
  three Criterion arithmetic means, not confidence intervals. Criterion:
  60 samples, 2 s warm-up, 20 s measurement.
- Full paired comparison, loads and caveats:
  [Performance changes for 1.2.0](#performance-changes-for-120-paired).

| Benchmark ID | Median mean / iteration | Range of means |
| --- | ---: | ---: |
| `batch_scan/three_patterns_2mib` | 47.752 µs | 47.254–47.972 µs |
| `linux_process_memory/current_process_64_bytes` | 249.187 ns | 247.137–249.667 ns |
| `map_calculation/full_difficulty_performance` | 11.936 µs | 11.849–12.216 µs |
| `map_calculation/gradual_all_objects` | 10.025 µs | 9.946–10.235 µs |
| `map_calculation/parse` | 1.553 µs | 1.539–1.584 µs |
| `map_calculation/unchanged_cache_key` | 10.140 ns | 10.130–10.352 ns |
| `publication/unfiltered_clients/1` | 2.0070 ms | 2.0066–2.0072 ms |
| `publication/unfiltered_clients/100` | 2.0109 ms | 2.0087–2.0143 ms |
| `publication/unfiltered_clients/1000` | 2.0492 ms | 2.0443–2.0521 ms |
| `raw_snapshot_reads/lazer_regular_fields` | 10.725 µs | 10.536–10.900 µs |
| `raw_snapshot_reads/stable_regular_fields` | 80.965 ns | 80.018–83.753 ns |
| `serialization_cache/stream_companion_bytes_clone` | 12.645 ns | 12.643–13.404 ns |
| `serialization_cache/v1_bytes_clone` | 12.659 ns | 12.643–12.869 ns |
| `serialization_cache/v2_bytes_clone` | 12.644 ns | 12.642–13.452 ns |
| `serialization_cache/v2_precise_bytes_clone` | 12.648 ns | 12.647–13.401 ns |
| `serializers_cold/stream_companion` | 1.661 µs | 1.621–1.663 µs |
| `serializers_cold/v1` | 1.439 µs | 1.435–1.464 µs |
| `serializers_cold/v2` | 2.892 µs | 2.839–3.040 µs |
| `serializers_cold/v2_precise` | 1.168 µs | 1.158–1.172 µs |
| `v2_subscription/distinct_plans/1` | 7.473 µs | 7.469–7.508 µs |
| `v2_subscription/distinct_plans/100` | 14.314 µs | 14.243–14.700 µs |
| `v2_subscription/distinct_plans/1000` | 82.628 µs | 81.678–84.289 µs |
| `v2_subscription/shared_plan/1` | 7.462 µs | 7.431–7.534 µs |
| `v2_subscription/shared_plan/100` | 12.622 µs | 12.614–12.717 µs |
| `v2_subscription/shared_plan/1000` | 57.901 µs | 57.856–60.150 µs |

### Native Windows hot paths

Measured on **2026-10-25**, baseline `c791512` and 1.2.0 `774fd0f`
(production-code-identical to `77080cd`, including `a44183b`).
Windows 11 IoT Enterprise LTSC Evaluation guest, Ryzen 9 9950X3D,
4 cores / 8 logical processors, 32 GiB RAM; Rust 1.96.1, native MSVC,
locked dependencies and `--no-default-features`. Three paired rounds
alternated baseline→candidate, candidate→baseline, baseline→candidate.
All six hot-path and six deterministic-reader invocations exited 0.

Each process was launched with `start "" /b /wait /affinity 4` (logical CPU 2).
The launch command is the hot-path pinning evidence: the immediate
child-affinity snapshots did not independently observe those short-lived
launcher children. Each source had an independent release target and each
source/round a fresh Criterion home. Criterion used the Windows workflow
defaults: 100 samples, 3 s warm-up and 5 s measurement, not the Linux
2 s / 20 s / 60-sample protocol. Values are medians of three arithmetic
means; parentheses span them, not their individual 95% confidence intervals.

| Native Windows benchmark | 1.1.0 median mean (range) | 1.2.0 median mean (range) | Change |
| --- | ---: | ---: | ---: |
| `batch_scan/three_patterns_2mib` | 193.705 µs (192.551–195.794) | 192.302 µs (174.802–194.273) | -0.7% |
| `map_calculation/full_difficulty_performance` | 16.198 µs (15.404–16.238) | 16.109 µs (15.593–16.149) | -0.6% |
| `map_calculation/gradual_all_objects` | 12.879 µs (12.730–13.115) | 12.767 µs (12.763–12.777) | -0.9% |
| `map_calculation/parse` | 2.111 µs (2.085–2.155) | 2.161 µs (2.157–2.185) | +2.4% |
| `map_calculation/unchanged_cache_key` | 15.677 ns (15.556–15.691) | 15.682 ns (15.539–15.736) | +0.0% |
| `publication/unfiltered_clients/1` | 17.343 ms (17.305–17.396) | 17.450 ms (17.402–17.467) | +0.6% |
| `publication/unfiltered_clients/100` | 17.488 ms (17.474–17.517) | 17.373 ms (17.366–17.421) | -0.7% |
| `publication/unfiltered_clients/1000` | 17.369 ms (17.272–17.489) | 17.478 ms (17.436–17.502) | +0.6% |
| `raw_snapshot_reads/lazer_regular_fields` | 10.911 µs (10.892–10.915) | 10.936 µs (10.915–10.952) | +0.2% |
| `raw_snapshot_reads/stable_regular_fields` | 109.812 ns (109.608–112.775) | 111.277 ns (109.040–112.168) | +1.3% |
| `serialization_cache/stream_companion_bytes_clone` | 13.949 ns (13.793–14.085) | 13.922 ns (13.817–14.102) | -0.2% |
| `serialization_cache/v1_bytes_clone` | 13.984 ns (13.803–14.673) | 13.686 ns (13.672–13.910) | -2.1% |
| `serialization_cache/v2_bytes_clone` | 13.879 ns (13.829–13.881) | 13.845 ns (13.784–14.153) | -0.2% |
| `serialization_cache/v2_precise_bytes_clone` | 13.996 ns (13.796–14.012) | 13.833 ns (13.817–14.178) | -1.2% |
| `serializers_cold/stream_companion` | 2.556 µs (2.555–2.574) | 1.275 µs (1.250–1.281) | -50.1% |
| `serializers_cold/v1` | 2.402 µs (2.308–2.426) | 1.031 µs (1.027–1.077) | -57.1% |
| `serializers_cold/v2` | 3.579 µs (3.573–3.635) | 2.403 µs (2.392–2.575) | -32.9% |
| `serializers_cold/v2_precise` | 2.093 µs (2.018–2.099) | 0.773 µs (0.768–0.776) | -63.1% |
| `v2_subscription/distinct_plans/1` | 10.049 µs (9.855–10.141) | 10.191 µs (10.090–10.502) | +1.4% |
| `v2_subscription/distinct_plans/100` | 18.097 µs (17.883–18.396) | 18.228 µs (18.051–18.263) | +0.7% |
| `v2_subscription/distinct_plans/1000` | 102.654 µs (100.972–102.741) | 103.223 µs (102.743–103.979) | +0.6% |
| `v2_subscription/shared_plan/1` | 10.178 µs (10.155–10.280) | 10.249 µs (10.182–10.305) | +0.7% |
| `v2_subscription/shared_plan/100` | 16.203 µs (16.054–16.313) | 16.171 µs (16.101–16.346) | -0.2% |
| `v2_subscription/shared_plan/1000` | 67.641 µs (67.379–67.741) | 67.739 µs (67.726–67.886) | +0.1% |
| `windows_process_memory/read_exact_64_bytes` | 391.438 ns (389.747–399.585) | 390.705 ns (390.237–394.285) | -0.2% |
| `windows_process_memory/read_many_contiguous_8x8_bytes` | 424.115 ns (422.765–429.345) | 425.731 ns (424.527–427.908) | +0.4% |
| `windows_process_memory/read_many_gapped_8x8_bytes` | 427.901 ns (427.541–429.693) | 426.667 ns (425.294–430.284) | -0.3% |
| `windows_process_memory/read_many_single_64_bytes` | 391.900 ns (391.197–393.262) | 390.587 ns (389.327–392.228) | -0.3% |
| `windows_process_memory/read_many_wide_8x8_bytes` | 1.027 µs (1.023–1.028) | 1.036 µs (1.029–1.042) | +0.9% |

All 29 IDs exist on both sides. Cold serializers are 32.9–63.1% faster,
with disjoint ranges; the other hot-path changes span -2.1% to +2.4%.
The deterministic synthetic reader below is not a live Windows lazer run:
live lazer was **not measured** because the client crashed in this VM environment.

| Deterministic reader | 1.1.0 median (range), ns | 1.2.0 median (range), ns |
| --- | ---: | ---: |
| Native self-process lazer precise | 19793.80 (19620.30–19795.30) | 19638.10 (19593.80–19642.30) |
| Stable idle decoder | 341.76 (337.98–342.97) | 335.10 (335.07–339.18) |
| Stable key-overlay decoder | 296.33 (293.03–297.56) | 297.54 (296.54–300.32) |

Reader values summarize three invocations, each reporting the median of
nine sample means. Counts match both sides: lazer 24 exact + 2 batch calls /
912 requested bytes; stable key-overlay 5 exact + 2 batch calls / 258 bytes.
Raw rounds, individual Criterion intervals, binary/source provenance and
`table.txt` / `summary.json` are retained under
`~/.local/share/vosu-1.2.0-evidence/Windows/benchmarks/`.

## Deterministic memory reader

Measured **2026-10-25** at final 1.2.0 source `77080cd`, on the Linux host
and pinned core described in [the paired run](#performance-changes-for-120-paired),
with default release features. Values are the median and range of three
invocations, each reporting the median of nine sample means. Lazer:
2,000 warm-up cycles, 1,000 per sample; stable: 20,000 warm-up cycles,
20,000 per sample. This harness is not Criterion.

| Workload | Time / cycle |
| --- | ---: |
| Native self-process lazer precise capture, 18 keys | 8.842 µs (8.680–8.952) |
| Stable idle decoder | 278.41 ns (271.19–282.35) |
| Stable key-overlay decoder | 234.41 ns (233.06–242.16) |

Counting fixtures: lazer 24 exact + 2 batch reads / 912 requested bytes per
cycle; stable key-overlay 5 exact + 2 batch reads / 258 bytes. These are API
counts, not syscall or allocation measurements. The separate
[precise-reader fixture](#precise-reader-fixture) comparison retains its
previous run and was not rerun here.

## Performance changes for 1.1.0 (paired)

Measured on 2026-10-17 in the Linux guest described under
[live-client runs](#live-client-runs), with Rust 1.96.1, locked offline
dependencies and default release features, no client or daemon running. The
1.0.2 code (`5b855e1`: the 1.0.2 sources plus the benchmark prerequisites, no
optimization) and the 1.1.0 release commit `c791512` ran alternately, three
rounds each, every benchmark process pinned to vCPU 2. Guest load averages
were 1.79 / 2.05 / 0.99 at the start, after the build.

Criterion used 2 s warm-up, 20 s measurement and 60 samples per case; values are
the median of the three Criterion means and ranges span them (µs unless marked).
In the 1.0.2 code the filtered benchmark's group sets 10 samples, 1 s warm-up
and 2 s measurement itself, which take precedence over these options; the
`c791512` version uses them. The precise reader is the median of fifteen
`memory_reader_bench` invocations.

| Benchmark | 1.0.2 code | `c791512` | 1.0.2 / now | 1.0.2 range | `c791512` range |
| --- | ---: | ---: | ---: | ---: | ---: |
| `regular_reader/lazer_gameplay` | 92.27 µs | 16.66 µs | 5.54× | 91.63–126.40 | 16.43–16.68 |
| `regular_reader/lazer_menu` | 108.19 µs | 29.79 µs | 3.63× | 107.58–123.73 | 29.42–29.86 |
| `regular_reader/stable_gameplay` | 45.71 µs | 12.70 µs | 3.60× | 45.27–45.71 | 12.42–12.73 |
| `regular_reader/stable_menu` | 49.11 µs | 10.58 µs | 4.64× | 48.74–49.35 | 10.31–10.63 |
| `memory_reader_bench` (lazer precise capture) | 10.64 µs | 8.789 µs | 1.21× | 10.44–10.97 | 8.687–8.959 |
| `filtered_projection/v2_identical/100` | 6.179 ms | 19.90 µs | 311× | 6.151–6.434 ms | 19.84–19.96 µs |
| `filtered_projection/v2_unique/100` | 6.950 ms | 31.71 µs | 219× | 6.796–7.194 ms | 31.61–31.72 µs |
| `http_v2/cold_connection` | 45.38 µs | 48.20 µs | 0.94× | 43.85–46.49 | 45.24–54.50 |
| `http_v2/keep_alive` | 5.575 µs | 5.710 µs | 0.98× | 5.540–5.618 | 5.634–5.735 |
| `http_v2/dispatch_only` | 364.3 ns | 381.4 ns | 0.96× | 354.9–374.4 | 380.3–382.1 |

Native backend calls per capture cycle, printed once by
`autoresearch_regular_reader` outside timing:

| Case | 1.0.2 code | `c791512` |
| --- | ---: | ---: |
| lazer gameplay | 268 | 28 |
| lazer menu | 314 | 72 |
| stable gameplay | 135 | 29 |
| stable menu | 148 | 21 |

- Batching reads whole spans instead of individual fields, so it requests more
  bytes while making far fewer native calls (lazer gameplay: 5,418 bytes in 28
  calls against 1,889 bytes in 268).
- `autoresearch_http_v2` is 2–6% slower in `c791512` on both platforms, within
  or near its ranges; 1.1.0 did not optimize the HTTP path.
- These are synthetic Linux fixtures. They do not measure live clients,
  network transport, whole-daemon CPU or memory. Native Windows results follow;
  the live effect on both platforms is under [live readers](#live-readers).
- The filtered harness exercises production projection and in-process
  WebSocket fan-out with changing snapshots, a paused Tokio clock and reference
  byte comparisons. Its broadcast-pass cache shares identical ordered filter
  plans (at most 64 variants / 1 MiB of payload), and top-level properties
  shared across distinct plans are bounded by a work budget.

Reproduce with the same benchmark binaries:

```sh
cargo bench --offline --locked --package vosu --bench hot_paths -- '^(serializers_cold/|v2_subscription/(distinct_plans|shared_plan)/1000$)'
cargo bench --offline --locked --package vosu --bench autoresearch_regular_reader
cargo bench --offline --locked --package vosu --bench autoresearch_filtered_projection -- '^filtered_projection/v2_(identical|unique)/100$'
cargo bench --offline --locked --package vosu --bench autoresearch_http_v2
cargo bench --offline --locked --package vosu --bench memory_reader_bench
```

### Native Windows

Measured on 2026-10-17 in the Windows guest described under
[live-client runs](#live-client-runs), with Rust 1.96.1, MSVC and
`--no-default-features`, as Windows releases are built. The 1.0.2 code
(`5b855e1`) and `c791512` ran alternately, three rounds each, every benchmark
process pinned to vCPU 2, with the Criterion options and statistics of the
Linux run above. Guest CPU load was 5% at the start.

| Benchmark | 1.0.2 code | `c791512` | 1.0.2 / now | 1.0.2 range | `c791512` range |
| --- | ---: | ---: | ---: | ---: | ---: |
| `memory_reader_bench` (lazer precise capture) | 19.10 µs | 19.14 µs | 1.00× | 18.97–24.02 | 18.97–19.46 |
| `filtered_projection/v2_identical/100` | 7.679 ms | 25.32 µs | 303× | 7.656–7.713 ms | 24.96–25.42 µs |
| `filtered_projection/v2_unique/100` | 7.688 ms | 38.64 µs | 199× | 7.646–7.900 ms | 37.95–38.70 µs |
| `v2_subscription/distinct_plans/1000` | 558.3 µs | 103.5 µs | 5.40× | 553.0–567.1 | 101.1–111.8 |
| `v2_subscription/shared_plan/1000` | 237.2 µs | 66.29 µs | 3.58× | 236.9–239.7 | 66.12–71.04 |
| `serializers_cold/v1` | 6.490 µs | 2.845 µs | 2.28× | 6.234–6.543 | 2.724–2.853 |
| `serializers_cold/v2` | 12.79 µs | 4.248 µs | 3.01× | 12.75–13.09 | 3.859–4.610 |
| `serializers_cold/v2_precise` | 3.855 µs | 2.642 µs | 1.46× | 3.847–3.886 | 2.411–2.667 |
| `serializers_cold/stream_companion` | 9.410 µs | 3.751 µs | 2.51× | 9.316–9.505 | 3.044–4.777 |
| `http_v2/cold_connection` | 359.5 µs | 370.1 µs | 0.97× | 358.1–363.5 | 359.7–387.8 |
| `http_v2/keep_alive` | 67.29 µs | 69.70 µs | 0.97× | 67.23–73.41 | 67.84–70.14 |
| `http_v2/dispatch_only` | 621.8 ns | 635.8 ns | 0.98× | 613.9–924.2 | 625.6–636.0 |

- A cold `/json/v2` request on a new connection costs about 360–370 µs in
  both builds; the background latency column of the
  [daemon table](#daemon-cpu-and-memory) is dominated by that path.
- `autoresearch_regular_reader` runs only on Linux in this revision, so this
  table has no regular-reader case. Windows `read_many` issues one
  `ReadProcessMemory` per run of ascending requests within one 4 KiB page,
  gapped ones included, plus a read for any short suffix, so one batched backend
  call can still cost several native reads; native read counts were not
  measured. The live Windows readers show the combined timing effect.

```powershell
cargo bench --locked --no-default-features --package vosu --target x86_64-pc-windows-msvc --bench hot_paths -- '^(serializers_cold/|v2_subscription/(distinct_plans|shared_plan)/1000$)'
cargo bench --locked --no-default-features --package vosu --target x86_64-pc-windows-msvc --bench autoresearch_filtered_projection -- '^filtered_projection/v2_(identical|unique)/100$'
cargo bench --locked --no-default-features --package vosu --target x86_64-pc-windows-msvc --bench autoresearch_http_v2
cargo bench --locked --no-default-features --package vosu --target x86_64-pc-windows-msvc --bench memory_reader_bench
```

## Performance changes for 1.2.0 (paired)

Measured **2026-10-25**, 1.1.0 release `c791512` from `git archive` versus
final 1.2.0 `77080cd` (production-code-identical to Windows-tested `774fd0f`,
including regression fix `a44183b`). This is the **Ryzen 9 9950X3D host**,
not the Linux guest used in the 1.1.0 paired section; absolute numbers
are not comparable with that earlier section.
Kernel `7.2.10-1-cachyos-bore`, Rust 1.96.1 (`31fca3adb`), performance
governor, boost enabled, default release features, `--locked`; VM off.

Both targets alternated 1.1.0→1.2.0 for three rounds per side, all processes
pinned with `taskset -c 15` on the frequency CCD (32 MiB L3, CPUs
8–15/24–31), not the 96 MiB V-cache CCD. **No pkexec, CPU/IRQ isolation,
cpuset restrictions or boot `isolcpus` were used.** Sibling 31 was unused
by us but other system work could run on either CPU. Conclusions depend
on the paired alternation, not absolute stability. This supersedes the
earlier partial regression-fix measurements.

The runner waited for the **one-minute** host load to fall below 1.0
before each side's round; five/fifteen-minute loads could remain higher.
Recorded 1 / 5 / 15 minute start/end loads:

| Round | Version | Start load | End load |
| --- | --- | --- | --- |
| 1 | 1.1.0 | 0.69 / 2.89 / 6.58 | 1.66 / 1.99 / 4.36 |
| 1 | 1.2.0 | 0.77 / 2.27 / 3.89 | 2.46 / 2.59 / 3.29 |
| 2 | 1.1.0 | 0.98 / 2.03 / 3.02 | 1.53 / 1.73 / 2.46 |
| 2 | 1.2.0 | 0.67 / 1.44 / 2.31 | 2.07 / 1.55 / 1.93 |
| 3 | 1.1.0 | 0.86 / 1.29 / 1.81 | 1.31 / 1.43 / 1.68 |
| 3 | 1.2.0 | 0.85 / 1.31 / 1.63 | 1.63 / 1.56 / 1.62 |

Both sources prepared their offset generator normally over the network.
Each side used an independent target and Criterion home under
`~/.cache/vosu-agents/BenchVosu/`: `target-110` / `target-120-final` and
`criterion-110-final` / `criterion-120-final`. Builds completed before
timing; no shared artifacts or cross-source Criterion baselines were used.

```sh
python scripts/prepare-offset-generator.py --rid linux-x64
# Set CARGO_TARGET_DIR and CRITERION_HOME to the corresponding side first.
taskset -c 15 cargo bench --locked --package vosu --bench hot_paths -- --warm-up-time 2 --measurement-time 20 --sample-size 60 --noplot
taskset -c 15 cargo bench --locked --package vosu --bench memory_reader_bench
```

Criterion settings match the earlier 1.1.0 paired run: 2 s warm-up,
20 s measurement, 60 samples. Values are medians of three arithmetic
means from `new/estimates.json`; ranges span those means, not confidence
intervals. Deterministic-reader rows instead summarize three invocation
medians of nine sample means. Change is `(1.2.0 / 1.1.0 − 1) × 100%`;
positive is slower.

| Benchmark | 1.1.0 | 1.2.0 | Change | 1.1.0 range | 1.2.0 range |
| --- | ---: | ---: | ---: | ---: | ---: |
| `batch_scan/three_patterns_2mib` | 46.498 µs | 47.752 µs | +2.70% | 45.814–49.307 µs | 47.254–47.972 µs |
| `linux_process_memory/current_process_64_bytes` | 254.575 ns | 249.187 ns | -2.12% | 245.844–255.168 ns | 247.137–249.667 ns |
| `map_calculation/full_difficulty_performance` | 12.197 µs | 11.936 µs | -2.14% | 11.865–12.566 µs | 11.849–12.216 µs |
| `map_calculation/gradual_all_objects` | 10.162 µs | 10.025 µs | -1.35% | 9.986–10.591 µs | 9.946–10.235 µs |
| `map_calculation/parse` | 1.609 µs | 1.553 µs | -3.50% | 1.559–1.624 µs | 1.539–1.584 µs |
| `map_calculation/unchanged_cache_key` | 10.079 ns | 10.140 ns | +0.61% | 10.074–10.197 ns | 10.130–10.352 ns |
| Lazer precise capture | 9.382 µs | 8.842 µs | -5.76% | 8.651–9.446 µs | 8.680–8.952 µs |
| Stable idle decoder | 308.350 ns | 278.410 ns | -9.71% | 280.660–310.750 ns | 271.190–282.350 ns |
| Stable key-overlay decoder | 247.480 ns | 234.410 ns | -5.28% | 234.750–250.320 ns | 233.060–242.160 ns |
| `publication/unfiltered_clients/1` | 2.0057 ms | 2.0070 ms | +0.07% | 2.0055–2.0063 ms | 2.0066–2.0072 ms |
| `publication/unfiltered_clients/100` | 2.0105 ms | 2.0109 ms | +0.02% | 2.0094–2.0117 ms | 2.0087–2.0143 ms |
| `publication/unfiltered_clients/1000` | 2.0460 ms | 2.0492 ms | +0.15% | 2.0414–2.0476 ms | 2.0443–2.0521 ms |
| `raw_snapshot_reads/lazer_regular_fields` | 9.885 µs | 10.725 µs | +8.50% | 9.871–9.890 µs | 10.536–10.900 µs |
| `raw_snapshot_reads/stable_regular_fields` | 81.707 ns | 80.965 ns | -0.91% | 77.419–83.869 ns | 80.018–83.753 ns |
| `serialization_cache/stream_companion_bytes_clone` | 13.081 ns | 12.645 ns | -3.34% | 12.780–13.536 ns | 12.643–13.404 ns |
| `serialization_cache/v1_bytes_clone` | 12.791 ns | 12.659 ns | -1.03% | 12.742–12.871 ns | 12.643–12.869 ns |
| `serialization_cache/v2_bytes_clone` | 13.086 ns | 12.644 ns | -3.38% | 12.789–13.492 ns | 12.642–13.452 ns |
| `serialization_cache/v2_precise_bytes_clone` | 13.080 ns | 12.648 ns | -3.30% | 12.780–13.576 ns | 12.647–13.401 ns |
| `serializers_cold/stream_companion` | 4.405 µs | 1.661 µs | -62.28% | 4.264–4.556 µs | 1.621–1.663 µs |
| `serializers_cold/v1` | 3.976 µs | 1.439 µs | -63.79% | 3.910–3.990 µs | 1.435–1.464 µs |
| `serializers_cold/v2` | 5.367 µs | 2.892 µs | -46.11% | 5.228–5.416 µs | 2.839–3.040 µs |
| `serializers_cold/v2_precise` | 3.599 µs | 1.168 µs | -67.56% | 3.399–3.602 µs | 1.158–1.172 µs |
| `v2_subscription/distinct_plans/1` | 7.676 µs | 7.473 µs | -2.64% | 7.504–8.039 µs | 7.469–7.508 µs |
| `v2_subscription/distinct_plans/100` | 14.698 µs | 14.314 µs | -2.61% | 14.408–15.273 µs | 14.243–14.700 µs |
| `v2_subscription/distinct_plans/1000` | 81.320 µs | 82.628 µs | +1.61% | 80.978–84.454 µs | 81.678–84.289 µs |
| `v2_subscription/shared_plan/1` | 7.785 µs | 7.462 µs | -4.15% | 7.326–8.053 µs | 7.431–7.534 µs |
| `v2_subscription/shared_plan/100` | 12.922 µs | 12.622 µs | -2.33% | 12.690–13.592 µs | 12.614–12.717 µs |
| `v2_subscription/shared_plan/1000` | 58.176 µs | 57.901 µs | -0.47% | 57.575–62.336 µs | 57.856–60.150 µs |

Both targets have identical benchmark sources on these commits: all
25 Criterion IDs and three reader metrics exist on both sides. There are
no one-sided IDs. Selected-field scheduling is not a separate ID in these
targets; no unmeasured new path is represented as a speedup. The full suite
includes all cold-serializer and subscription cases affected by `a44183b`.

Cold serializers are **46.11–67.56% faster**, with disjoint ranges.
Their owned `iter_batched` closures include snapshot destruction, not
construction. `a44183b` shares the immutable default settings graph and
copies on actual changes, removing per-snapshot graph teardown; it also
skips stable's unnecessary lazer-only speed lookup. Thus these are not
pure encoder-only timings. Subscription changes now span -4.15% to +1.61%,
all with overlapping ranges: per-lane reference counts avoid the former
full per-client demand scan on each publication.

**One remaining >5% regression:** the zero-filled raw lazer fixture is
**+8.50%**, with disjoint ranges, 9.871–9.890 µs versus 10.536–10.900 µs.
The populated deterministic reader's apparent 5.28–9.71% gains have
overlapping ranges; do not claim resolved improvements from those medians.
Quick profiles (120 GDB-interrupted stacks per version, approximately
50 ms apart, separately symbolized optimized binaries) locate the cost in
invalid-root recovery: `resolve_game_base_by_scan`, fixture
`copy_from_slice` and AVX2 `memchr` scanning. Fixture-copy libc frames
accounted for 62/120 samples on each side; the AVX2 finder had 50/120
baseline versus 53/120 candidate samples. An untimed single-iteration
Criterion test-mode trace counted **six scan calls on both sides**.
Thus the regressing workload is repeated failed-root scan/copy work,
not populated live capture or an increased scan count. The quick profile
does **not** establish the precise cause of the extra 0.840 µs; binary
layout or background interference remains an **[INFERENCE]**, not a
demonstrated explanation. No code was changed to hide this result.
Raw `profile-*-raw-lazer.log`, `profile-summary.json` and
`scan-trace-*.log` are retained with the paired evidence.

Raw per-round Criterion JSON/stdout, deterministic-reader stdout,
environment/load records, `summary.json` and full `table.txt` are under
`/tmp/vosu-r4-evidence/Bench120-final/`. This run is taskset-only and does
not reuse the earlier isolated run's raw results.

## Released tosu comparison

These runs compare vosu with released tosu **4.26.2** (tag
`03202868c73c38938b65cce3f39b3c50e49b0fe4`, commit
`b21b4526d8db9ce124f5a8bf475ef5421837f5b7`), the latest GitHub release when
measured. Current Windows stable live-client results compare vosu 1.1.0
`c791512` and 1.2.0 `774fd0f` on **2026-10-25**. Linux live-client results
retain `c791512` on **2026-10-17**; the separate tosu hot-path comparison
retains vosu `v1.0.0` on **2026-10-11**, and the precise-reader fixture retains
`9c47ccbe6d0a70da4b723bf7f5c30b89489251a9` on **2026-10-12**.

### Live-client runs

The retained Linux guest results used `c791512` with Rust 1.96.1, default
release features (`vosu` SHA-256 `dfe8537b790ca96e…`) and published tosu
`v4.26.2` (`fa22300bcdc88c3b…`). The current Windows native MSVC binaries
use `--no-default-features`: 1.1.0 SHA-256 `69a49ebe3fcc680a…`, 1.2.0
`81550909c8072543…`, and published tosu `0b365384f2454245…`.
Runs used fresh per-run copies with matched settings:
`POLL_RATE` 150, `PRECISE_DATA_POLL_RATE` 10, PP calculation and key overlay
on, loopback-only listeners, automatic updates off, tosu's in-game overlay off,
and a warmed vosu lazer offsets cache. vosu's opt-in `ENABLE_HIT_OFFSETS` and
`ENABLE_MANIA_KEY_HIT_ERRORS` kept their default `false` except in the
[hit-offsets row](#daemon-cpu-and-memory). The workload was one silent 25-minute
map with 6001 circles (checksum `954a4cf2a177d1d008d52c80b1b371a3`) played by
Autoplay in the osu! ruleset on lazer `2026.1005.0` and stable `b20260924.1`.

The host is a Ryzen 9 9950X3D with the RTX 5090 passed through to one guest at
a time:

- **Windows guest:** Windows 11 IoT Enterprise LTSC 26100, 8 logical
  processors. Current stable `b20260924.1` (SHA-256 `3adc2d001aacced5…`)
  played the 6,001-circle map in Autoplay, AT only, 1× speed. Live lazer
  was **not measured for 1.2.0**: both the current and older task-copied
  clients crashed in this VM environment, including OpenGL/Direct3D11
  trials. Synthetic self-process lazer fixtures do not close that gap.
- **Linux guest:** Arch Linux, `linux-zen` 7.1.4, 8 vCPUs pinned to the V-cache
  CCD, 24 GiB, glibc 2.43. Clients ran on a headless labwc (Vulkan on the RTX
  5090) through its Xwayland. Lazer used the release AppImage with the profile
  created for the 2026-10-11 runs. Stable used a copy of the host's osu-winello
  Wine 11.8 Staging build,
  prefix and program folder, with Wine's ALSA audio driver because its
  PulseAudio driver never advanced the audio clock in this guest. The vosu
  copies carried the file capabilities `vosu --setup` grants on an
  eBPF-capable kernel (the eBPF process-event backend was selected); the tosu
  copies carried `cap_sys_ptrace`, which Yama `ptrace_scope=1` requires. Both
  ran as the desktop user.

The tosu-side harness is task-owned and outside this repository. It bundles
tosu commit `b21b4526d8db9ce124f5a8bf475ef5421837f5b7` with benchmark entry
points that call the unchanged production functions.

#### Hot paths

Measured on idle guests, before any client was started. vosu used Criterion
on the release commit. The tosu harness gives each case at least 3 s of
warm-up and 100 equally weighted samples of about 50 ms, reporting the mean and
a 95% Student t interval; that is Criterion-like, not Criterion. Values are
means per iteration.

| Benchmark ID | Windows tosu | Windows vosu | tosu / vosu | Linux tosu | Linux vosu | tosu / vosu |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `batch_scan/three_patterns_2mib` | 1.603 ms | 195.934 µs | 8.18× | 1.069 ms | 62.559 µs | 17.09× |
| `linux_process_memory/current_process_64_bytes` | — | — | — | 991.577 ns | 304.825 ns | 3.25× |
| `map_calculation/full_difficulty_performance` | 332.630 µs | 17.344 µs | 19.18× | 227.061 µs | 14.172 µs | 16.02× |
| `map_calculation/gradual_all_objects` | 437.981 µs | 14.608 µs | 29.98× | 252.084 µs | 14.567 µs | 17.31× |
| `map_calculation/parse` | 41.892 µs | 2.309 µs | 18.14× | 37.245 µs | 1.628 µs | 22.87× |
| `raw_snapshot_reads/lazer_regular_fields` | 400.973 µs | 5.895 µs | 68.02× | 206.320 µs | 6.082 µs | 33.92× |
| `raw_snapshot_reads/stable_regular_fields` | 18.604 µs | 337.148 ns | 55.18× | 8.095 µs | 219.466 ns | 36.89× |
| `serializers_cold/stream_companion` | 9.558 µs | 11.500 µs | 0.83× | 7.673 µs | 7.193 µs | 1.07× |
| `serializers_cold/v1` | 11.319 µs | 8.084 µs | 1.40× | 9.269 µs | 4.671 µs | 1.98× |
| `serializers_cold/v2` | 16.640 µs | 13.639 µs | 1.22× | 13.710 µs | 11.781 µs | 1.16× |
| `serializers_cold/v2_precise` | 888.088 ns | 4.866 µs | 0.18× | 863.276 ns | 3.528 µs | 0.24× |
| `windows_process_memory/read_exact_64_bytes` | 1.035 µs | 400.119 ns | 2.59× | — | — | — |
| `windows_process_memory/read_many_contiguous_8x8_bytes` | 8.169 µs | 436.657 ns | 18.71× | — | — | — |
| `windows_process_memory/read_many_gapped_8x8_bytes` | 8.156 µs | 439.031 ns | 18.58× | — | — | — |
| `windows_process_memory/read_many_single_64_bytes` | 1.028 µs | 399.726 ns | 2.57× | — | — | — |
| `windows_process_memory/read_many_wide_8x8_bytes` | 8.207 µs | 1.053 µs | 7.79× | — | — | — |

The rows exercise the closest tosu production path, not identical work:

- tosu's map calculation goes through its native lazer calculator
  (`0.6.1-20260729-main.0`), including the JavaScript bridge.
- tosu's raw-snapshot cases take error and not-ready paths on the zeroed
  fixture, and its lazer case rescans because the fixture fails its GameBase
  check.
- `batch_scan` calls tosu's inner scanner through an adapter, not its
  process-wide scan.
- tosu has no batched read API, so the read-many cases issue eight reads, and
  every read allocates a returned buffer.
- tosu's precise serializer surfaces four keys; vosu's fixture has 18. The
  serializer inputs differ in general.

On Windows the vosu `serializers_cold/v1` and `stream_companion` intervals are
wide (6.3–11.6 µs and 9.6–15.1 µs). The vosu-only groups have no tosu
counterpart. Its `publication` cases took about 18 ms on Windows and 2.1 ms on
Linux; that difference was not investigated. All results are in the evidence
bundle.

#### Live readers

With no daemon running, each reader attached directly to the playing client.
On Windows stable, five triples alternated 1.1.0→1.2.0→tosu and the reverse
order against the same game. These live readers were not CPU-pinned.
Linux retains five alternating vosu/tosu invocations per client.
Each invocation runs 100 warm-up cycles, then nine samples of 1,000 (vosu)
or 100 (tosu) cycles, and reports the median sample mean. vosu uses
`examples/live_reader_bench.rs` and the production capture adapters; tosu
uses production initialization with polling disabled. Values are medians
of five invocation medians with their range, excluding setup. All 15 current
Windows invocations exited 0, with zero read errors or not-ready counts.

| Client | Product | Regular cycle | Precise cycle | Errors | Setup |
| --- | --- | ---: | ---: | ---: | ---: |
| Windows stable | vosu 1.1.0 | 152.285 µs (150.958–165.797) | 27.906 µs (26.974–29.275) | 0 | excluded |
| Windows stable | vosu 1.2.0 | 157.412 µs (155.088–164.594) | 27.729 µs (27.553–28.283) | 0 | excluded |
| Windows stable | tosu 4.26.2 | 566.976 µs (517.107–589.800) | 46.774 µs (42.512–49.436) | 0 | excluded |
| Linux lazer | vosu | 20.2 µs (17.8–21.3) | 9.1 µs (8.5–10.2) | 0 | 61 ms |
| Linux lazer | tosu | 147.1 µs (138.1–183.1) | 32.8 µs (31.8–39.8) | 0 | 3972 ms |
| Linux stable (Wine) | vosu | 33.7 µs (31.8–36.3) | 8.5 µs (7.6–9.4) | 0 | 22 ms |
| Linux stable (Wine) | tosu | 189.6 µs (182.2–232.7) | 13.9 µs (11.8–16.9) | 0 | 286 ms |

Windows 1.2.0 changes are +3.37% regular and -0.64% precise versus 1.1.0,
with overlapping invocation ranges; no statistically significant change is
established. Windows lazer has no current live result. Windows reader evidence:
`~/.local/share/vosu-1.2.0-evidence/Windows/live-campaign/reader-summary.json`,
`reader-results/` and `staged.json`.

- On Linux lazer, tosu's ranked-play read returned the production not-ready
  sentinel on every regular cycle (counted, not an error). The cycle continued
  through settings, gameplay, leaderboard and user reads, 504 reads per cycle
  (542 on Windows). The two readers do not read identical field sets, so cycle
  times compare equivalent production work, not identical work.
- In the Linux guest both clients played into a PipeWire null sink, and tosu's
  harness changed in one place: it no longer requires a 32-bit ELF for
  `--client stable`. osu-winello's Wine runs 32-bit `osu!.exe` inside a 64-bit
  ELF process, and tosu's own instance manager treats every Linux `osu!.exe` as
  stable without that check. Guest load averages were 0.15–1.98 during the
  Linux runs.
- 1.1.0 reads stable replay frames on every precise cycle, in batches, to
  derive keys and hit errors in every ruleset. On one stable process with no
  daemon running, five alternating pairs of the 1.1.0 candidate `fe3201c`
  (before that change) and `c791512` put the stable precise cycle at 20.22 µs
  against 24.74 µs on Windows (1.22×) and 6.68 µs against 8.42 µs on Linux
  (1.26×); regular cycles did not change. The lazer precise cycle is unaffected.

##### Linux regular-cycle reads by field group

Separate instrumented invocations (three per product, medians shown) counted
read calls per regular cycle by field group while the same Autoplay plays ran.
vosu used a throwaway example, rebuilt against `c791512`, that wraps the
production process backend with counters and calls the reader methods in
`SourceRegularReader`'s order; its per-group sums were within one call of the
production capture's own count. tosu's harness
wrapped each `read(name)` call. The two counts are not the same unit, and
neither is a syscall count:

- **vosu backend calls** count `read_exact` and `read_many` calls on the
  process backend. Each is normally one `process_vm_readv`, but a short read or
  the mapped-region fallback can issue more, and each call also runs the
  process-generation check.
- **tosu API calls** count `Process` method calls. A string read
  (`readSharpString`) is one call but two native reads, length then data
  (`tsprocess/lib/functions.cc`), so its native-read count is higher by the
  number of non-empty strings read.
- **Bytes** for vosu include strings. tosu's harness adds no bytes for string
  reads, so its byte columns are lower bounds, and its totals are unknown.

Times include counter overhead.

| Client | Group | vosu backend calls | vosu bytes | vosu time | tosu API calls | tosu bytes (excl. strings) | tosu time |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Linux lazer | global | 10.2 | 1,830 | 6.7 µs | 54 | ≥ 398 | 17.9 µs |
| Linux lazer | menu | 0 | 0 | 0.0 µs | 14 | ≥ 96 | 5.2 µs |
| Linux lazer | mp3 length | 0 | 0 | 0.0 µs | 6 | ≥ 48 | 2.0 µs |
| Linux lazer | ranked play + room | 0 | 0 | 0.1 µs | 4 | ≥ 32 | 1.6 µs |
| Linux lazer | **settings** | **9** | 4,330 | **8.3 µs** | **243** | ≥ 1,170 | **67.8 µs** |
| Linux lazer | gameplay | 4 | 1,269 | 3.3 µs | 115 | ≥ 616 | 38.3 µs |
| Linux lazer | leaderboard | 0 | 0 | 0.0 µs | 61 | ≥ 317 | 19.8 µs |
| Linux lazer | user | 1 | 44 | 0.6 µs | 7 | ≥ 52 | 2.7 µs |
| Linux lazer | total | 24 | 7,472 | 19.2 µs | 504 | — | 155.0 µs |
| Linux stable (Wine) | global | 4 | 828 | 4.9 µs | 22 | ≥ 68 | 15.0 µs |
| Linux stable (Wine) | menu | 4 | 828 | 2.6 µs | 9 | ≥ 28 | 5.6 µs |
| Linux stable (Wine) | mp3 length | 3 | 16 | 1.3 µs | 3 | ≥ 16 | 1.7 µs |
| Linux stable (Wine) | **settings** | **4.8** | 11,369 | **16.4 µs** | **224** | ≥ 744 | **110.7 µs** |
| Linux stable (Wine) | gameplay | 5 | 336 | 2.7 µs | 28 | ≥ 104 | 24.2 µs |
| Linux stable (Wine) | leaderboard | 0 | 0 | 0.0 µs | 3 | ≥ 12 | 2.7 µs |
| Linux stable (Wine) | user | 4 | 448 | 2.2 µs | 17 | ≥ 69 | 8.6 µs |
| Linux stable (Wine) | total | 24 | 13,816 | 32.2 µs | 306 | — | 169.4 µs |

Batched captures closed the settings gap: on Linux lazer vosu reads settings in
9 backend calls (8.3 µs) against tosu's 243 API calls (67.8 µs), and the whole
regular cycle in 24 calls (19.2 µs) against 504 (155.0 µs). On stable the cycle
is 24 calls (32.2 µs) against 306 (169.4 µs). A batch reads whole spans, so vosu
requests more bytes than tosu while making far fewer calls. Groups with no vosu
calls reuse fields that an earlier batch in the same capture already read; for
example, lazer's batched gameplay read also decodes the personal leaderboard
entry. Fractional counts are per-cycle averages. tosu's string reads still hide
a second native read each, so its call counts understate its native reads.

#### Daemon CPU and memory

A psutil collector (7.0.0 on Windows, 7.2.2 on Linux) sampled each daemon's
process tree once per second after 30 s of warm-up, for three 45 s windows per
mode. In **background** mode each window made one `/json/v2` request at its
start and one at its end on a new connection. In **consumer** mode it requested
`/json/v2` at 10 Hz over keep-alive. Every window had to keep the same map,
client, ruleset and mods, a monotonic play clock and non-decreasing score, and
consumer windows had to hold their cadence; all windows were accepted. Values are
medians over the three windows. **vosu alone** ran by itself; the other
columns come from vosu and tosu running together. On Linux the collector ran as
root because processes with file capabilities are not dumpable.

| Client | Mode | vosu alone CPU | vosu CPU | tosu CPU | vosu RSS | tosu RSS | vosu private | tosu private | `/json/v2` latency vosu / tosu |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Linux lazer | background | 0.89% | 0.84% | 1.42% | 29.8 MB | 266.2 MB | 25.9 MB | 256.1 MB | 0.77 / 3.25 ms |
| Linux lazer | consumer | 0.96% | 0.96% | 2.53% | 31.0 MB | 276.1 MB | 27.2 MB | 266.1 MB | 0.25 / 2.61 ms |
| Linux stable (Wine) | background | 1.00% | 0.96% | 1.04% | 39.7 MB | 266.6 MB | 35.8 MB | 257.7 MB | 0.80 / 4.42 ms |
| Linux stable (Wine) | consumer | 1.11% | 1.07% | 2.27% | 40.5 MB | 274.7 MB | 36.6 MB | 265.8 MB | 0.29 / 3.06 ms |

Current **Windows stable** whole-daemon results (2026-10-25) replace the
earlier Windows rows. The psutil 7.0.0 collector excludes the game process
and uses three complete windows per invocation: 30 s warm-up, 45 s measurement.
It validates playing state, client/map/mode/mod/rate identity, advancing clock
and nondecreasing score. Background mode makes only boundary requests;
consumer mode requests `/json/v2` at 10 Hz keep-alive. Rows are medians across
the three window values, with min–max ranges. RSS and private commit are
window means in **MiB**, not the older table's MB. These campaigns are not
claimed CPU-pinned. CPU is percent of one core.

| Stable live campaign | Product | CPU, % one core | RSS, MiB | Private commit, MiB | API latency, ms |
| --- | --- | ---: | ---: | ---: | ---: |
| 1.1 alone, background | vosu 1.1 | 1.91 (1.77–1.91) | 48.69 (48.24–48.80) | 13.94 (13.52–14.05) | 20.066 (2.755–23.961) |
| 1.2 alone, background | vosu 1.2 | 1.56 (1.32–1.63) | 49.11 (49.02–49.13) | 14.42 (14.14–14.43) | 15.786 (2.932–22.638) |
| 1.1 alone, consumer | vosu 1.1 | 2.22 (1.84–2.43) | 49.19 (48.88–49.50) | 14.58 (14.42–14.89) | 0.469 (0.449–0.508) |
| 1.2 alone, consumer | vosu 1.2 | 2.15 (2.01–2.29) | 49.16 (48.96–49.76) | 14.59 (14.49–15.29) | 0.451 (0.438–0.462) |
| 1.1 + tosu, background | vosu 1.1 | 1.60 (1.25–2.53) | 49.76 (49.35–49.85) | 15.18 (15.13–15.45) | 8.812 (4.939–10.788) |
| 1.1 + tosu, background | tosu 4.26.2 | 1.87 (1.74–2.22) | 145.76 (144.95–146.35) | 129.94 (129.25–130.84) | 7.515 (7.139–8.260) |
| 1.2 + tosu, background | vosu 1.2 | 1.46 (1.35–1.94) | 48.03 (48.00–48.11) | 13.24 (13.18–13.31) | 7.994 (7.458–8.574) |
| 1.2 + tosu, background | tosu 4.26.2 | 1.84 (1.39–2.33) | 143.16 (142.77–143.62) | 126.01 (124.98–126.60) | 5.465 (5.385–16.826) |

These eight product rows come from six accepted invocations; concurrent
tosu consumer campaigns are not included. Background API latency has too
few boundary samples to stand in for sustained-consumer latency.
An original 1.2+tosu campaign crossed the map end and was excluded in full;
its next retry failed preflight because AT was deselected. After reselecting
AT, `06c` passed all three complete windows; rejected values are not used.
Windows lazer remains unmeasured because of the environment crash.
Raw accepted/rejected evidence, frozen configurations and identities are in
`~/.local/share/vosu-1.2.0-evidence/Windows/live-campaign/`
(`daemon-results/`, `daemon-summary.json`, `daemon-table.txt`).

**Hit-offsets cost.** A separate vosu-alone run on Linux lazer set
`ENABLE_HIT_OFFSETS = true` in the run's `vosu.toml` before warm-up, on a fresh
Autoplay of the same map in the same lazer process, and confirmed that
`/json/v2` published `play.hitOffsets` (432 entries at the start, 5,853 at the
end); all six windows were accepted. Compared with the default vosu-alone run:

| Linux lazer, vosu alone | Mode | CPU | RSS | Private | `/json/v2` latency |
| --- | --- | ---: | ---: | ---: | ---: |
| `ENABLE_HIT_OFFSETS` off (default) | background | 0.89% | 37.1 MB | 33.3 MB | 0.56 ms |
| `ENABLE_HIT_OFFSETS` on | background | 0.91% | 37.2 MB | 33.4 MB | 0.69 ms |
| `ENABLE_HIT_OFFSETS` off (default) | consumer | 0.96% | 38.5 MB | 34.7 MB | 0.24 ms |
| `ENABLE_HIT_OFFSETS` on | consumer | 1.16% | 38.9 MB | 35.0 MB | 0.39 ms |

With the setting on, the precise reader also reads each hit's offset, and
every `/json/v2` response carries the whole offset list, which grows through
the play. The consumer difference was not split between those two costs.

- CPU is a percentage of one logical core.
- RSS is the Windows working set or Linux resident set.
- Private memory is Windows private commit or Linux `Private_Clean +
  Private_Dirty + Private_Hugetlb`, so it is not comparable across platforms.
- Background latency covers only two cold requests per window, each on a new
  connection, so it mostly measures connection setup.
- tosu 4.26.2 publishes empty play data (score, combo, hits, mods) for
  osu-winello stable when started from another directory: it derives the game
  folder from Wine's `D:\osu!.exe` command line as `.`, resolves it against its
  own working directory, cannot open the `.osu` file, and skips gameplay
  updates while the map metadata is not ready. The Linux stable rows started
  tosu with its working directory set to the osu! folder, without changing
  tosu; vosu started from its own run directory.

Evidence is retained on the measurement host under
`~/.cache/vosu-rebench-20261017/`: `windows-results/live-c791512/` and
`linux-results-c791512/` (launch identities, collector configurations,
per-window samples, reader logs, per-group breakdowns, the `fe3201c`/`c791512`
stable pairs and the hit-offsets run), `windows-results/paired-c791512/` and
`linux-results-c791512/paired/` (Criterion output and logs), and `windows/` and
`linux/` (the guest scripts, including `regular_read_breakdown.rs`). The
harnesses come from `~/.cache/vosu-1.0.0-live-20261011/` (the tosu harness with
its mapping and checksums, and the hot-path evidence) and
`~/.cache/vosu-linux-matrix-20261013/` (the Linux launcher with `--tosu-cwd`,
the patched tosu harness and `patch_tosu_harness.py`).

### Precise-reader fixture

The timed fixture is a resolved lazer precise state/clock, 18 keys, and an
empty hit-error list in 1 MiB of synthetic memory. Each invocation runs 2,000
warm-up cycles, then nine samples of 1,000 cycles, and reports the median
sample mean; the table shows the median and range of seven invocations. Setup
is excluded. The released tosu reader requires direct initialization of its
private resolved screen and game-base pointers; it has no
`synchronize_precise` method. Its key-overlay method has no mode argument.
Neither adaptation replaces its production decoding or native process-read
methods. The harness's `lazer.ts`, `memory/index.ts`, and `package.json` are
byte-identical to the tag.

Linux runs three builds: tosu, vosu with default features
(`linux-process-events`, as released; binary
`7596d58f0ecdc2fc890b6777bfa138bda46e91ba23ec39fc9398659da4a75a63`), and vosu
with `--no-default-features` (no pidfd; binary
`ff09c73f35438ce770224cf9273dad7656ead5c9bc083954aa558c5150a85fd0`). Each of
seven rounds uses a different ordering of the three; the host and VM ran the
same binaries. Windows alternates tosu and vosu over seven pairs, using a vosu
binary built natively from a `git archive` of the commit with Rust 1.96.1,
MSVC, and `--no-default-features`, which matches Windows releases (SHA-256
`76d96590393ee0f6ab90bc5131aa8c3a413b6e0c42fe2b6d9ca966bd9956ddfe`).

Values are ns/cycle: the median and range of seven per-invocation medians.

| Platform | Released tosu | vosu, released features | vosu, no pidfd | tosu / vosu (released features) |
| --- | ---: | ---: | ---: | ---: |
| Linux host | 31,592.93 (31,391.21–32,622.03) | 10,400.07 (10,247.08–10,524.60) | 98,872.83 (97,568.95–101,575.26) | 3.038 |
| Linux VM | 30,892.93 (30,265.51–40,899.35) | 10,682.06 (10,634.45–11,082.38) | 108,066.99 (105,012.04–125,809.50) | 2.892 |
| Native Windows, run 1 | 80,970.10 (71,042.80–89,286.50) | 19,214.90 (18,945.50–28,592.30) | — | 4.214 |
| Native Windows, run 2 | 74,846.30 (70,784.00–80,413.40) | 20,353.70 (18,888.90–22,006.00) | — | 3.677 |

- **Linux:** the released-feature build is 2.9–3.0 times faster than tosu.
  Without the feature, vosu is 9.5 (host) and 10.1 (VM) times slower than
  with it: each native read also reads `/proc/<pid>/stat` twice for the
  process-generation check.
- **Windows:** run 1 started minutes after the guest booted, while Microsoft
  Defender (`MsMpEng`) was the busiest guest process; run 2 followed it and
  has the narrower spread.
- **Counts:** every invocation reported tosu 156 typed reads and 1,020 bytes,
  and vosu 24 exact plus 2 batched reads and 912 bytes per cycle.

The Windows tosu harness reads its own `offsets.json`
(`b6b225409523f7d86911dcd3c2e3f3d16f2f1158f4c9d5e1c4e6a99f45ba0379`) rather
than the Linux harness copy of vosu's `assets/offsets.json`; the fixture is
written and decoded with the same file, and every key, time, and empty-list
assertion passed.

Tosu counts wrapped production typed methods during timing; vosu's accounting
uses a separate counting reader, while its timed path reads native
self-process memory. Counts are instrumented API calls, not syscall/allocation
measurements. Tosu asserts each key's values and the empty error array; the
vosu benchmark asserts readiness, time, key count, and error index, not all of
those same values. Do not generalize these fixture timings to whole-daemon CPU.

Environments: host Linux `7.2.9-1-cachyos-bore` on a Ryzen 9 9950X3D,
performance governor, start load 2.46 / 2.28 / 1.80; VM kernel
**7.1.4-zen1-1-zen**, glibc **2.43**, eight pinned vCPUs, start load
0.28 / 0.07 / 0.02; Windows 11 IoT Enterprise LTSC 10.0.26100 guest. The two
VMs ran one after the other, never together. No CPU isolation, privilege
elevation, or system-policy changes were used.

Evidence is under `~/.cache/vosu-precise-rerun-20261012/`:
`readers-linux-host/` and `readers-linux-vm/` (manifest and 42 logs each),
`readers-windows/` and `readers-windows-2/` (`results.json` and 14 logs each),
`run-readers-linux.py`, and `windows/` (staged source archive, build script, build log).

## Windows live precise cadence

Measured on **2026-09-29**, separately from the synthetic and hosted
benchmarks: an attested native Windows stable capture on the local VM measured
precise WebSocket delivery at **87.0 Hz upstream and 90.0 Hz in vosu**, with
zero gaps exceeding the fixed 450 ms stall threshold. The active windows
contained 2,089 and 2,166 observations, respectively.

The corrected daemon holds a balanced 1 ms Windows multimedia timer-resolution
request while each instance worker clock exists. Reproduce using the pinned
artifacts, 150 ms regular polling, 25 ms HTTP capture interval, and the complete
menu → play → pause → result → close procedure in the
[verification guide](verification.md#session-clock-and-cadence-remediation).
The evidence is `remediation-7c0j_0ys/clock/stable-minimum-skew-report.json`
under the retained verification directory named there. This capture still
fails one-to-one HTTP reference coverage; cadence measurements do not establish
full parity, timer deadline guarantees, or reduced CPU usage.

## Native Windows coverage

The [native Windows results](#native-windows-hot-paths) above were measured
locally in the Windows guest. The manual
[`Windows benchmarks`](../../.github/workflows/benchmarks-windows.yml) workflow
can reproduce both targets on `windows-latest` with Rust 1.96.1 and native
MSVC, including all five `windows_process_memory` cases. It uploads logs,
exit codes, revision, runner details and Criterion samples as
`windows-benchmarks-<commit>` (14 days). Hosted or local Windows timings are
not directly comparable with Linux results.

Reproduce in PowerShell:

```powershell
python scripts/prepare-offset-generator.py --rid win-x64
cargo bench --locked --no-default-features --package vosu --target x86_64-pc-windows-msvc --bench hot_paths
cargo bench --locked --no-default-features --package vosu --target x86_64-pc-windows-msvc --bench memory_reader_bench
```

## Selected-field reader cadence (2026-10-23)

Reproduce the Linux-only offline measurement, alone in an optimized test binary:

```sh
python scripts/prepare-offset-generator.py --rid linux-x64
cargo test --release --locked --lib subscription_cadence_cpu_and_wakeups -- --ignored --nocapture --test-threads=1
```

The fixture establishes real subscriptions through `V2SubscriptionHub`, feeds its
aggregated demand through `PollIntervalUpdates::Config`, and runs the real worker
threads/reducer scheduling with the existing synthetic reader/transition harness.
No process discovery or live-client attachment occurs. Each case warms up for
250 ms, then measures five seconds. Captures count purposeful reader wakeups;
`CLOCK_PROCESS_CPUTIME_ID` measures process CPU, expressed as percent of one core.
Configured regular/precise intervals are 150/10 ms and debounce is 1 ms.

| Established selection | Regular captures/s | Precise captures/s | CPU seconds | CPU % |
| --- | ---: | ---: | ---: | ---: |
| None | 6.598 | 98.776 | 0.026204 | 0.524 |
| `/settings/mode`, `/client`, `/folders/skin`, `/beatmap/checksum` | 6.599 | 99.591 | 0.029537 | 0.591 |
| `/keys`, play context | 6.598 | 967.241 | 0.088958 | 1.778 |
| Empty pointer (whole document) | 923.627 | 963.020 | 0.150711 | 3.014 |

The slow subscription leaves both readers at baseline, while the precise-only
subscription accelerates only its producing lane. Whole-document selection still
requests both lanes and serves as a control equivalent to the former all-lanes
high-rate demand; it is **not** a measurement of a historical binary. Unit tests
also cover gameplay demand becoming dormant outside play, parent selectors,
multiple clients, lane release on disconnect, and the independent status/audio
clock lanes.

These are one local run's synthetic scheduling/snapshot costs, not native client
memory-reading, live pp, full-daemon, or rendering benchmarks. The small CPU
difference between no subscriber and slow selection is not evidence of a meaningful
regression; the reader rates show no cadence increase. No live-game CPU improvement
or guaranteed 1 kHz delivery is claimed.

