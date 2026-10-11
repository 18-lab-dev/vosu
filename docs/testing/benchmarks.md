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

## 1.0.0 release results — 2026-10-10

Both runs measured the released `v1.0.0` commit
`57b322bae580198d0c0fae7360615348b85a47e8` with Rust 1.96.1, locked
dependencies, and Criterion defaults (100 samples, 3 s warm-up, 5 s
measurement). Values are arithmetic means and 95% confidence intervals from
Criterion's `new/estimates.json`. Linux and hosted Windows timings come from
different machines and are not comparable with each other.

### Linux

- Ryzen 9 9950X3D, CachyOS Linux `7.2.9-1-cachyos-bore`, performance governor,
  no CPU pinning or isolation. Clean checkout of the release commit. One-minute
  host load was 6.35 at the start and 5.56 at the end; the idle Windows VM was
  running on the same host.
- Commands are those under [Reproduce](#reproduce), with a fresh `CRITERION_HOME`.
  All 25 hot-path cases and the memory-reader target completed successfully.

The 2026-09-09 column repeats the [earlier full-suite results](#prior-full-suite-local-results),
which measured an uncommitted tree on kernel `7.2.2`. Changes are descriptive,
not controlled before/after comparisons.

| Benchmark ID | 2026-09-09 mean | 1.0.0 mean | 1.0.0 95% confidence interval | Change |
| --- | ---: | ---: | ---: | ---: |
| `batch_scan/three_patterns_2mib` | 48.736 µs | 55.880 µs | 55.244–56.538 µs | +14.7% |
| `linux_process_memory/current_process_64_bytes` | 168.792 ns | 323.225 ns | 320.984–326.511 ns | +91.5% |
| `map_calculation/full_difficulty_performance` | 12.699 µs | 12.235 µs | 12.199–12.273 µs | -3.7% |
| `map_calculation/gradual_all_objects` | 10.448 µs | 10.311 µs | 10.277–10.349 µs | -1.3% |
| `map_calculation/parse` | 1.646 µs | 1.565 µs | 1.558–1.571 µs | -4.9% |
| `map_calculation/unchanged_cache_key` | 11.079 ns | 10.284 ns | 10.254–10.317 ns | -7.2% |
| `publication/unfiltered_clients/1` | 2.070 ms | 2.052 ms | 2.046–2.058 ms | -0.9% |
| `publication/unfiltered_clients/100` | 2.084 ms | 2.055 ms | 2.047–2.063 ms | -1.4% |
| `publication/unfiltered_clients/1000` | 2.151 ms | 2.155 ms | 2.140–2.170 ms | +0.2% |
| `raw_snapshot_reads/lazer_regular_fields` | 85.332 µs | 5.998 µs | 5.930–6.058 µs | -93.0% |
| `raw_snapshot_reads/stable_regular_fields` | 572.740 ns | 207.013 ns | 206.329–207.740 ns | -63.9% |
| `serialization_cache/stream_companion_bytes_clone` | 13.004 ns | 12.951 ns | 12.896–13.033 ns | -0.4% |
| `serialization_cache/v1_bytes_clone` | 12.873 ns | 13.108 ns | 13.039–13.192 ns | +1.8% |
| `serialization_cache/v2_bytes_clone` | 13.741 ns | 12.906 ns | 12.873–12.947 ns | -6.1% |
| `serialization_cache/v2_precise_bytes_clone` | 13.838 ns | 13.051 ns | 12.999–13.107 ns | -5.7% |
| `serializers_cold/stream_companion` | 7.040 µs | 7.037 µs | 6.136–8.727 µs | -0.0% |
| `serializers_cold/v1` | 6.896 µs | 5.048 µs | 4.907–5.195 µs | -26.8% |
| `serializers_cold/v2` | 10.224 µs | 11.471 µs | 10.168–13.911 µs | +12.2% |
| `serializers_cold/v2_precise` | 4.588 µs | 3.851 µs | 3.747–3.961 µs | -16.1% |
| `v2_subscription/distinct_plans/1` | 8.414 µs | 7.899 µs | 7.871–7.930 µs | -6.1% |
| `v2_subscription/distinct_plans/100` | 56.661 µs | 43.196 µs | 43.060–43.335 µs | -23.8% |
| `v2_subscription/distinct_plans/1000` | 595.639 µs | 442.293 µs | 439.052–445.461 µs | -25.7% |
| `v2_subscription/shared_plan/1` | 8.132 µs | 7.899 µs | 7.869–7.929 µs | -2.9% |
| `v2_subscription/shared_plan/100` | 38.781 µs | 26.987 µs | 26.902–27.079 µs | -30.4% |
| `v2_subscription/shared_plan/1000` | 354.395 µs | 231.969 µs | 228.720–236.355 µs | -34.5% |

- **Selected-field subscriptions** are 24–35% faster at 100 and 1,000 clients.
- **`raw_snapshot_reads/lazer_regular_fields`** reflects the upstream 2359305
  offset layout and reader change, not a live-reader speedup; see the
  [update comparison](#upstream-2359305-update-comparison).
- **`linux_process_memory/current_process_64_bytes`** matches the roughly
  300 ns cost recorded after the [security generation checks](#security-generation-checks--2026-10-02)
  were added around each native read.
- Cold-serializer intervals stay wide, especially StreamCompanion and v2;
  treat their changes as noise until isolated reruns confirm them.

Deterministic memory reader, one invocation, medians of nine sample means:

| Workload | Time / cycle |
| --- | ---: |
| Native self-process lazer precise capture, 18 keys | 102.677 µs |
| Stable idle decoder | 237.07 ns |
| Stable key-overlay decoder | 233.21 ns |

Counting fixtures: lazer 24 exact + 2 batch reads / 912 requested bytes per
cycle; stable key-overlay 5 exact + 2 batch reads / 258 bytes. The lazer
fixture changed with the upstream 2359305 update (it was 26 + 3 reads / 570
bytes on 2026-09-09), so its earlier 8.315 µs is not a baseline. The new value
is in line with the 109.7 µs Linux VM result in the
[precise-reader microbenchmarks](#precise-reader-microbenchmarks).

### Hosted Windows

The [`Windows benchmarks`](../../.github/workflows/benchmarks-windows.yml) workflow
ran on `windows-latest` (run `38101226442`): Windows Server 2025 Datacenter,
runner image `win25-vs2026` `20260925.250.1`, AMD EPYC 7763 with one core / two
logical processors, 8 GiB memory, native MSVC. Both targets exited 0; all 29
hot-path cases completed, including the five `windows_process_memory` cases.
Shared hosted runners add noise, and the uploaded artifact expires after 14 days.

| Benchmark ID | Mean / iteration | 95% confidence interval |
| --- | ---: | ---: |
| `batch_scan/three_patterns_2mib` | 334.575 µs | 328.828–341.114 µs |
| `map_calculation/full_difficulty_performance` | 35.928 µs | 35.016–36.960 µs |
| `map_calculation/gradual_all_objects` | 28.963 µs | 28.173–29.889 µs |
| `map_calculation/parse` | 5.190 µs | 5.049–5.352 µs |
| `map_calculation/unchanged_cache_key` | 21.580 ns | 21.006–22.260 ns |
| `publication/unfiltered_clients/1` | 13.790 ms | 13.332–14.225 ms |
| `publication/unfiltered_clients/100` | 14.027 ms | 13.618–14.425 ms |
| `publication/unfiltered_clients/1000` | 14.193 ms | 13.815–14.565 ms |
| `raw_snapshot_reads/lazer_regular_fields` | 12.997 µs | 12.720–13.329 µs |
| `raw_snapshot_reads/stable_regular_fields` | 826.833 ns | 807.058–849.349 ns |
| `serialization_cache/stream_companion_bytes_clone` | 20.804 ns | 19.121–23.119 ns |
| `serialization_cache/v1_bytes_clone` | 18.739 ns | 18.613–18.884 ns |
| `serialization_cache/v2_bytes_clone` | 18.718 ns | 18.576–18.883 ns |
| `serialization_cache/v2_precise_bytes_clone` | 18.871 ns | 18.724–19.041 ns |
| `serializers_cold/stream_companion` | 22.272 µs | 21.504–23.163 µs |
| `serializers_cold/v1` | 13.098 µs | 12.605–13.787 µs |
| `serializers_cold/v2` | 46.428 µs | 44.031–49.110 µs |
| `serializers_cold/v2_precise` | 9.901 µs | 7.835–13.861 µs |
| `v2_subscription/distinct_plans/1` | 24.712 µs | 23.841–25.867 µs |
| `v2_subscription/distinct_plans/100` | 136.162 µs | 132.872–139.805 µs |
| `v2_subscription/distinct_plans/1000` | 1.427 ms | 1.361–1.496 ms |
| `v2_subscription/shared_plan/1` | 32.855 µs | 31.394–34.589 µs |
| `v2_subscription/shared_plan/100` | 53.584 µs | 52.139–55.301 µs |
| `v2_subscription/shared_plan/1000` | 378.174 µs | 371.335–385.717 µs |
| `windows_process_memory/read_exact_64_bytes` | 486.939 ns | 477.184–498.445 ns |
| `windows_process_memory/read_many_contiguous_8x8_bytes` | 541.494 ns | 533.365–550.998 ns |
| `windows_process_memory/read_many_gapped_8x8_bytes` | 569.688 ns | 554.376–586.886 ns |
| `windows_process_memory/read_many_single_64_bytes` | 480.538 ns | 469.901–493.419 ns |
| `windows_process_memory/read_many_wide_8x8_bytes` | 1.284 µs | 1.259–1.314 µs |

Memory reader on the hosted runner: lazer precise capture 48,561.60 ns,
stable idle 1,264.90 ns, stable key overlay 632.37 ns per cycle, with the same
read counts as Linux.

## 1.0.0 against tosu 4.26.2 in live clients — 2026-10-11

Both products ran from their published release assets: vosu `v1.0.0`
(`vosu.exe` SHA-256 `4cd54b1ea0fa1fce…`, Linux `vosu` `644130e344aa0f74…`) and
tosu `v4.26.2` (`tosu.exe` `0b365384f2454245…`, Linux `tosu`
`fa22300bcdc88c3b…`). Each run used fresh per-run copies with matched settings:
`POLL_RATE` 150, `PRECISE_DATA_POLL_RATE` 10, PP calculation and key overlay
on, loopback-only listeners, automatic updates off, tosu's in-game overlay off,
and a warmed vosu lazer offsets cache. The workload was one silent 25-minute
map with 6001 circles (checksum `954a4cf2a177d1d008d52c80b1b371a3`) played by
Autoplay in the osu! ruleset on lazer `2026.1005.0` and stable `b20260924.1`.

The host is a Ryzen 9 9950X3D with the RTX 5090 passed through to one guest at
a time:

- **Windows guest:** Windows 11 IoT Enterprise LTSC 26100, 8 logical
  processors. Stable ran from the user's installation, whose `Songs` and
  `Skins` links were pointed at a local folder for the run and then restored;
  its databases and per-user configuration were verified unchanged afterward.
- **Linux guest:** Arch Linux, `linux-zen` 7.1.4, 8 vCPUs pinned to the V-cache
  CCD, 24 GiB, glibc 2.43. Clients ran on a headless labwc (Vulkan on the RTX
  5090) through its Xwayland. Lazer used the release AppImage with a fresh
  profile. Stable used a copy of the host's osu-winello Wine 11.8 Staging build,
  prefix and program folder, with Wine's ALSA audio driver because its
  PulseAudio driver never advanced the audio clock in this guest. The vosu
  copies carried the file capabilities `vosu --setup` grants on an
  eBPF-capable kernel (the eBPF process-event backend was selected); the tosu
  copies carried `cap_sys_ptrace`, which Yama `ptrace_scope=1` requires. Both
  ran as the desktop user.

The tosu-side harness is task-owned and outside this repository. It bundles
tosu commit `b21b4526d8db9ce124f5a8bf475ef5421837f5b7` with benchmark entry
points that call the unchanged production functions.

### Hot paths

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

### Live readers

With no daemon running, each reader attached directly to the playing client,
alternating vosu and tosu for five invocations each. An invocation runs
100 warm-up cycles, then nine samples of 1000 (vosu) or 100 (tosu) cycles, and
reports the median sample mean. vosu used `examples/live_reader_bench.rs`,
which calls the daemon's regular and precise capture adapters after the
production reducer has chosen its capture plan. tosu used its production
`Process`, pattern resolution and instance initialization with polling
disabled. The table shows the median of the five invocation medians and their
range; setup is excluded from the cycle times.

| Client | Product | Regular cycle | Precise cycle | Errors | Setup |
| --- | --- | ---: | ---: | ---: | ---: |
| Windows lazer | vosu | 572.4 µs (566.4–574.8) | 32.7 µs (32.0–33.1) | 0 | 74 ms |
| Windows lazer | tosu | 793.7 µs (788.1–799.4) | 167.7 µs (163.3–172.7) | 0 | 1559 ms |
| Windows stable | vosu | 190.6 µs (184.9–208.2) | 20.5 µs (20.2–22.6) | 0 | 88 ms |
| Windows stable | tosu | 492.0 µs (477.6–512.6) | 42.1 µs (40.8–48.9) | 0 | 735 ms |
| Linux lazer | vosu | 193.6 µs (189.1–215.6) | 10.7 µs (10.3–11.8) | 0 | 69 ms |
| Linux lazer | tosu | 145.3 µs (138.5–178.0) | 32.3 µs (31.9–36.9) | 0 | 3634 ms |
| Linux stable (Wine) | vosu | 90.9 µs (one invocation) | 7.7 µs (one invocation) | 0 | — |

- On Linux lazer, tosu's ranked-play read returned the production not-ready
  sentinel on every regular cycle (counted, not an error). The cycle continued
  through settings, gameplay, leaderboard and user reads, 504 reads per cycle
  (542 on Windows). The two readers do not read identical field sets, so cycle
  times compare equivalent production work, not identical work.
- The Linux stable row has one vosu invocation because the tosu harness refused
  the client: its own guard requires a 32-bit process for `--client stable`,
  and osu-winello's Wine reported the `osu!.exe` process as 64-bit. That guard
  belongs to the harness, not to tosu. Separately, the tosu 4.26.2 daemon found
  this client and published its state and clock, but play score, combo, hits
  and mods stayed empty; the cause was not investigated. No Linux stable
  head-to-head was run.
- The first Windows stable invocation failed because the example rejected memory
  errors in its first setup capture, which still uses the default menu plan.
  The example now requires only the plan-selected final setup capture to be
  clean; the timed code did not change. Windows lazer used the earlier build.

### Daemon CPU and memory

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
| Windows lazer | background | 1.98% | 2.15% | 2.78% | 44.1 MB | 91.1 MB | 7.0 MB | 80.0 MB | 6.86 / 4.55 ms |
| Windows lazer | consumer | 2.29% | 1.67% | 2.85% | 44.4 MB | 93.9 MB | 7.4 MB | 83.3 MB | 0.37 / 0.87 ms |
| Windows stable | background | 1.46% | 1.77% | 2.01% | 50.3 MB | 154.0 MB | 13.4 MB | 136.2 MB | 17.34 / 13.78 ms |
| Windows stable | consumer | 1.63% | 1.74% | 3.75% | 51.5 MB | 162.6 MB | 14.7 MB | 157.8 MB | 0.44 / 3.71 ms |
| Linux lazer | background | 1.02% | 1.00% | 1.42% | 35.8 MB | 265.4 MB | 31.9 MB | 255.2 MB | 0.73 / 3.67 ms |
| Linux lazer | consumer | 1.07% | 1.13% | 2.51% | 37.5 MB | 273.6 MB | 33.6 MB | 263.4 MB | 0.27 / 2.70 ms |
| Linux stable (Wine) | background | 0.80% | — | — | 29.7 MB | — | 25.8 MB | — | 0.65 ms / — |
| Linux stable (Wine) | consumer | 0.93% | — | — | 30.4 MB | — | 26.5 MB | — | 0.25 ms / — |

- CPU is a percentage of one logical core.
- RSS is the Windows working set or Linux resident set.
- Private memory is Windows private commit or Linux `Private_Clean +
  Private_Dirty + Private_Hugetlb`, so it is not comparable across platforms.
- Background latency covers only two cold requests per window, each on a new
  connection, so it mostly measures connection setup.

Evidence (launch identities, collector configurations, per-window samples,
reader logs, Criterion output and the tosu harness with its mapping and
checksums) is retained under `~/.cache/vosu-1.0.0-live-20261011/` on the
measurement host.

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

The [1.0.0 hosted Windows results](#hosted-windows) are the first recorded run. The manual
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
