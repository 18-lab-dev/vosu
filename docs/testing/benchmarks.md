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
and checks the process generation by reading `/proc/<pid>/stat` before and
after every native read, so the timing measures that fallback instead
(about ten times slower; see the [precise-reader fixture](#precise-reader-fixture)).
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

Measured on **2026-10-10**. Both runs measured the released `v1.0.0` commit
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
  All 25 hot-path cases completed successfully.

| Benchmark ID | Mean / iteration | 95% confidence interval |
| --- | ---: | ---: |
| `batch_scan/three_patterns_2mib` | 55.880 µs | 55.244–56.538 µs |
| `linux_process_memory/current_process_64_bytes` | 323.225 ns | 320.984–326.511 ns |
| `map_calculation/full_difficulty_performance` | 12.235 µs | 12.199–12.273 µs |
| `map_calculation/gradual_all_objects` | 10.311 µs | 10.277–10.349 µs |
| `map_calculation/parse` | 1.565 µs | 1.558–1.571 µs |
| `map_calculation/unchanged_cache_key` | 10.284 ns | 10.254–10.317 ns |
| `publication/unfiltered_clients/1` | 2.052 ms | 2.046–2.058 ms |
| `publication/unfiltered_clients/100` | 2.055 ms | 2.047–2.063 ms |
| `publication/unfiltered_clients/1000` | 2.155 ms | 2.140–2.170 ms |
| `raw_snapshot_reads/lazer_regular_fields` | 5.998 µs | 5.930–6.058 µs |
| `raw_snapshot_reads/stable_regular_fields` | 207.013 ns | 206.329–207.740 ns |
| `serialization_cache/stream_companion_bytes_clone` | 12.951 ns | 12.896–13.033 ns |
| `serialization_cache/v1_bytes_clone` | 13.108 ns | 13.039–13.192 ns |
| `serialization_cache/v2_bytes_clone` | 12.906 ns | 12.873–12.947 ns |
| `serialization_cache/v2_precise_bytes_clone` | 13.051 ns | 12.999–13.107 ns |
| `serializers_cold/stream_companion` | 7.037 µs | 6.136–8.727 µs |
| `serializers_cold/v1` | 5.048 µs | 4.907–5.195 µs |
| `serializers_cold/v2` | 11.471 µs | 10.168–13.911 µs |
| `serializers_cold/v2_precise` | 3.851 µs | 3.747–3.961 µs |
| `v2_subscription/distinct_plans/1` | 7.899 µs | 7.871–7.930 µs |
| `v2_subscription/distinct_plans/100` | 43.196 µs | 43.060–43.335 µs |
| `v2_subscription/distinct_plans/1000` | 442.293 µs | 439.052–445.461 µs |
| `v2_subscription/shared_plan/1` | 7.899 µs | 7.869–7.929 µs |
| `v2_subscription/shared_plan/100` | 26.987 µs | 26.902–27.079 µs |
| `v2_subscription/shared_plan/1000` | 231.969 µs | 228.720–236.355 µs |

- **`linux_process_memory/current_process_64_bytes`** includes the pidfd
  process-generation check before and after each native read.
- Cold-serializer intervals are wide, especially StreamCompanion and v2;
  confirm differences in them with isolated reruns.

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

## Deterministic memory reader

Measured on **2026-10-12** at `9c47ccbe6d0a70da4b723bf7f5c30b89489251a9` on the
Linux host described under [vosu hot paths](#vosu-hot-paths), with the Linux
release features. Values are the median and range of seven invocations, each
reporting the median of nine sample means. Lazer: 2,000 warm-up cycles,
1,000 per sample; stable: 20,000 warm-up cycles, 20,000 per sample.

| Workload | Time / cycle |
| --- | ---: |
| Native self-process lazer precise capture, 18 keys | 10.400 µs (10.247–10.525) |
| Stable idle decoder | 237.55 ns (233.13–247.80) |
| Stable key-overlay decoder | 232.90 ns (231.24–238.97) |

Counting fixtures: lazer 24 exact + 2 batch reads / 912 requested bytes per
cycle; stable key-overlay 5 exact + 2 batch reads / 258 bytes. These are API
counts, not syscall or allocation measurements. The lazer capture is also the
vosu side of the [precise-reader fixture](#precise-reader-fixture), which
records the same run.

## Released tosu comparison

These runs compare vosu with released tosu **4.26.2** (tag
`03202868c73c38938b65cce3f39b3c50e49b0fe4`, commit
`b21b4526d8db9ce124f5a8bf475ef5421837f5b7`), the latest GitHub release when
measured. The live-client runs used vosu `v1.0.0` on **2026-10-11**, except the
Linux live-reader rows and the Linux stable daemon rows, which used vosu
`v1.0.2` on **2026-10-13**; the precise-reader fixture used vosu
`9c47ccbe6d0a70da4b723bf7f5c30b89489251a9` on **2026-10-12**.

### Live-client runs

Both products ran from their published release assets: vosu `v1.0.0`
(`vosu.exe` SHA-256 `4cd54b1ea0fa1fce…`, Linux `vosu` `644130e344aa0f74…`),
vosu `v1.0.2` for the 2026-10-13 Linux rows (Linux `vosu` `ac91dc559265fa67…`)
and tosu `v4.26.2` (`tosu.exe` `0b365384f2454245…`, Linux `tosu`
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
| Linux lazer | vosu | 203.6 µs (188.5–210.3) | 11.0 µs (10.6–11.9) | 0 | 72 ms |
| Linux lazer | tosu | 165.3 µs (153.6–182.4) | 36.3 µs (33.4–48.2) | 0 | 3984 ms |
| Linux stable (Wine) | vosu | 104.8 µs (85.1–130.5) | 7.4 µs (6.6–10.6) | 0 | 20 ms |
| Linux stable (Wine) | tosu | 156.8 µs (142.1–187.2) | 11.7 µs (10.6–18.6) | 0 | 235 ms |

- On Linux lazer, tosu's ranked-play read returned the production not-ready
  sentinel on every regular cycle (counted, not an error). The cycle continued
  through settings, gameplay, leaderboard and user reads, 504 reads per cycle
  (542 on Windows). The two readers do not read identical field sets, so cycle
  times compare equivalent production work, not identical work.
- The Linux rows were measured on 2026-10-13 with vosu `v1.0.2`, the guest
  configuration above (lazer reused the 2026-10-11 profile; both clients played
  into a PipeWire null sink), and tosu's harness changed in one place: it no
  longer requires a 32-bit ELF for `--client stable`. osu-winello's Wine runs
  32-bit `osu!.exe` inside a 64-bit ELF process, and tosu's own instance manager
  treats every Linux `osu!.exe` as stable without that check. Guest load
  averages were 0.47–2.98 during the runs, and the guest's QEMU housekeeping
  threads shared host CPUs 8–15/24–31 with two concurrent host benchmark
  sessions.
- The first Windows stable invocation failed because the example rejected memory
  errors in its first setup capture, which still uses the default menu plan.
  The example now requires only the plan-selected final setup capture to be
  clean; the timed code did not change. Windows lazer used the earlier build.

##### Linux regular-cycle reads by field group

Separate instrumented invocations (three per product, medians shown) counted
read calls per regular cycle by field group while the same Autoplay plays ran.
vosu used a throwaway example that wraps the production process backend with
counters and calls the reader methods in `SourceRegularReader`'s order; its
per-group totals matched the production capture's own totals. tosu's harness
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
| Linux lazer | global (with per-cycle game-base check) | 57 | 545 | 25.3 µs | 54 | ≥ 398 | 17.2 µs |
| Linux lazer | menu | 10 | 132 | 4.3 µs | 14 | ≥ 96 | 5.0 µs |
| Linux lazer | mp3 length | 4 | 32 | 1.8 µs | 6 | ≥ 48 | 2.0 µs |
| Linux lazer | ranked play + room | 4 | 32 | 1.7 µs | 4 | ≥ 32 | 1.5 µs |
| Linux lazer | **settings** | **363** | 2,193 | **154.5 µs** | **243** | ≥ 1,170 | **63.9 µs** |
| Linux lazer | gameplay | 35 | 828 | 16.9 µs | 115 | ≥ 616 | 35.2 µs |
| Linux lazer | leaderboard | 23 | 453 | 10.2 µs | 61 | ≥ 317 | 18.5 µs |
| Linux lazer | user | 4 | 28 | 1.8 µs | 7 | ≥ 52 | 2.5 µs |
| Linux lazer | total | 500 | 4,242 | 218.4 µs | 504 | — | 146.9 µs |
| Linux stable (Wine) | global | 27 | 112 | 12.4 µs | 22 | ≥ 68 | 12.9 µs |
| Linux stable (Wine) | **settings** | **88** | 2,107 | **53.8 µs** | **224** | ≥ 744 | **93.3 µs** |
| Linux stable (Wine) | gameplay + leaderboard | 18 | 112 | 9.6 µs | 31 | ≥ 116 | 21.6 µs |
| Linux stable (Wine) | menu, mp3 length, user | 32 | 327 | 14.4 µs | 29 | ≥ 113 | 13.3 µs |
| Linux stable (Wine) | total | 164 | 2,649 | 91.1 µs | 306 | — | 140.5 µs |

On Linux lazer the regular-cycle gap sits in the settings group: vosu spends
154.5 µs on 363 backend calls, none batched, against tosu's 63.9 µs on 243 API
calls. Every other group is equal or cheaper in vosu. These counts do not
establish how many more fields vosu reads, because tosu's string reads hide a
second native read each. The settings group's time per call also differs
(about 0.43 µs against 0.26 µs), but this measurement does not isolate why:
the per-read generation check is one candidate, and it needs syscall tracing
or an isolated benchmark before it is treated as the cause. On stable, vosu
batches 85 of its settings reads into three calls and is ahead.

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
| Windows lazer | background | 1.98% | 2.15% | 2.78% | 44.1 MB | 91.1 MB | 7.0 MB | 80.0 MB | 6.86 / 4.55 ms |
| Windows lazer | consumer | 2.29% | 1.67% | 2.85% | 44.4 MB | 93.9 MB | 7.4 MB | 83.3 MB | 0.37 / 0.87 ms |
| Windows stable | background | 1.46% | 1.77% | 2.01% | 50.3 MB | 154.0 MB | 13.4 MB | 136.2 MB | 17.34 / 13.78 ms |
| Windows stable | consumer | 1.63% | 1.74% | 3.75% | 51.5 MB | 162.6 MB | 14.7 MB | 157.8 MB | 0.44 / 3.71 ms |
| Linux lazer | background | 1.02% | 1.00% | 1.42% | 35.8 MB | 265.4 MB | 31.9 MB | 255.2 MB | 0.73 / 3.67 ms |
| Linux lazer | consumer | 1.07% | 1.13% | 2.51% | 37.5 MB | 273.6 MB | 33.6 MB | 263.4 MB | 0.27 / 2.70 ms |
| Linux stable (Wine) | background | 0.84% | 0.84% | 0.93% | 36.0 MB | 241.8 MB | 32.0 MB | 234.9 MB | 0.95 / 4.24 ms |
| Linux stable (Wine) | consumer | 0.96% | 0.93% | 2.04% | 37.6 MB | 246.6 MB | 33.6 MB | 239.8 MB | 0.27 / 2.86 ms |

- CPU is a percentage of one logical core.
- RSS is the Windows working set or Linux resident set.
- Private memory is Windows private commit or Linux `Private_Clean +
  Private_Dirty + Private_Hugetlb`, so it is not comparable across platforms.
- Background latency covers only two cold requests per window, each on a new
  connection, so it mostly measures connection setup.
- The Linux stable rows are from 2026-10-13 (vosu `v1.0.2`); the other rows are
  from 2026-10-11 (vosu `v1.0.0`). tosu 4.26.2 publishes empty play data
  (score, combo, hits, mods) for osu-winello stable when started from another
  directory: it derives the game folder from Wine's `D:\osu!.exe` command line
  as `.`, resolves it against its own working directory, cannot open the
  `.osu` file, and skips gameplay updates while the map metadata is not ready.
  These rows started tosu with its working directory set to the osu! folder,
  without changing tosu; vosu started from its own run directory.

Evidence (launch identities, collector configurations, per-window samples,
reader logs, Criterion output and the tosu harness with its mapping and
checksums) is retained under `~/.cache/vosu-1.0.0-live-20261011/` and, for the
2026-10-13 rows, `~/.cache/vosu-linux-matrix-20261013/` (`results/`,
`readers/`, the patched launcher and harness, `patch_tosu_harness.py`, and
`regular_read_breakdown.rs`) on the measurement host.

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

The [hosted Windows results](#hosted-windows) come from the manual
[`Windows benchmarks`](../../.github/workflows/benchmarks-windows.yml) workflow,
which runs both targets on `windows-latest` with Rust 1.96.1 and native MSVC, including
all five `windows_process_memory` cases. It uploads logs, exit codes, revision,
runner details and Criterion samples as `windows-benchmarks-<commit>` (14 days).
Hosted timings are not directly comparable with the local Linux results.

Reproduce in PowerShell:

```powershell
python scripts/prepare-offset-generator.py --rid win-x64
cargo bench --locked --no-default-features --package vosu --target x86_64-pc-windows-msvc --bench hot_paths
cargo bench --locked --no-default-features --package vosu --target x86_64-pc-windows-msvc --bench memory_reader_bench
```
