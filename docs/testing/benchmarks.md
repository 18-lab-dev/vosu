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

- **`linux_process_memory/current_process_64_bytes`** was measured with the
  historical before-and-after pidfd generation checks. Current code performs
  one post-read check; the recorded timing has not been rerun for that change.
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
| `v2_subscription/distinct_plans/1000` | 409.1 µs | 85.58 µs | 4.78× | 409.0–435.8 | 85.27–86.55 |
| `v2_subscription/shared_plan/1000` | 209.2 µs | 61.28 µs | 3.41× | 208.4–209.2 | 59.84–61.41 |
| `serializers_cold/v1` | 4.450 µs | 2.647 µs | 1.68× | 4.437–5.939 | 2.639–2.818 |
| `serializers_cold/v2` | 10.54 µs | 3.953 µs | 2.67× | 10.41–10.69 | 3.907–4.158 |
| `serializers_cold/v2_precise` | 3.914 µs | 2.718 µs | 1.44× | 3.881–3.990 | 2.690–2.954 |
| `serializers_cold/stream_companion` | 6.527 µs | 3.230 µs | 2.02× | 6.501–6.539 | 3.162–3.250 |
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

## Released tosu comparison

These runs compare vosu with released tosu **4.26.2** (tag
`03202868c73c38938b65cce3f39b3c50e49b0fe4`, commit
`b21b4526d8db9ce124f5a8bf475ef5421837f5b7`), the latest GitHub release when
measured. The live-client runs used the vosu 1.1.0 release commit `c791512` on
**2026-10-17**; the hot paths used vosu `v1.0.0` on **2026-10-11**, and the
precise-reader fixture used vosu `9c47ccbe6d0a70da4b723bf7f5c30b89489251a9` on
**2026-10-12**.

### Live-client runs

vosu was built from `c791512` with Rust 1.96.1 and each platform's release
features: Windows `vosu.exe` (SHA-256 `ba897fdc5120f85b…`) natively with MSVC
and `--no-default-features`, Linux `vosu` (`dfe8537b790ca96e…`) in the Linux
guest with default features. tosu ran from its published `v4.26.2` assets
(`tosu.exe` `0b365384f2454245…`, Linux `tosu` `fa22300bcdc88c3b…`). Each run used
fresh per-run copies with matched settings:
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
  processors. Lazer ran from the user's installation and profile, signed out,
  in a maximized window (it opened windowed and did not switch to borderless).
  Stable ran from the user's installation, whose `Songs` and `Skins` links were
  pointed at a local folder for the run and then restored; its databases and
  per-user configuration were verified unchanged afterward.
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
| Windows lazer | vosu | 148.0 µs (145.1–149.8) | 30.0 µs (29.8–31.0) | 0 | 71 ms |
| Windows lazer | tosu | 737.0 µs (729.2–741.5) | 151.2 µs (150.2–152.5) | 0 | 1521 ms |
| Windows stable | vosu | 132.7 µs (131.7–135.3) | 23.9 µs (23.8–24.3) | 0 | 85 ms |
| Windows stable | tosu | 456.2 µs (448.6–472.9) | 40.7 µs (40.6–42.0) | 0 | 589 ms |
| Linux lazer | vosu | 20.2 µs (17.8–21.3) | 9.1 µs (8.5–10.2) | 0 | 61 ms |
| Linux lazer | tosu | 147.1 µs (138.1–183.1) | 32.8 µs (31.8–39.8) | 0 | 3972 ms |
| Linux stable (Wine) | vosu | 33.7 µs (31.8–36.3) | 8.5 µs (7.6–9.4) | 0 | 22 ms |
| Linux stable (Wine) | tosu | 189.6 µs (182.2–232.7) | 13.9 µs (11.8–16.9) | 0 | 286 ms |

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
| Windows lazer | background | 1.67% | 1.67% | 2.78% | 41.7 MB | 88.8 MB | 7.1 MB | 78.6 MB | 9.90 / 10.45 ms |
| Windows lazer | consumer | 1.49% | 1.70% | 2.64% | 42.2 MB | 91.2 MB | 7.9 MB | 82.7 MB | 0.30 / 0.68 ms |
| Windows stable | background | 1.49% | 1.53% | 1.63% | 49.7 MB | 148.9 MB | 14.7 MB | 131.5 MB | 4.83 / 8.00 ms |
| Windows stable | consumer | 1.70% | 1.81% | 3.19% | 50.9 MB | 152.7 MB | 16.0 MB | 135.7 MB | 0.36 / 3.43 ms |
| Linux lazer | background | 0.89% | 0.84% | 1.42% | 29.8 MB | 266.2 MB | 25.9 MB | 256.1 MB | 0.77 / 3.25 ms |
| Linux lazer | consumer | 0.96% | 0.96% | 2.53% | 31.0 MB | 276.1 MB | 27.2 MB | 266.1 MB | 0.25 / 2.61 ms |
| Linux stable (Wine) | background | 1.00% | 0.96% | 1.04% | 39.7 MB | 266.6 MB | 35.8 MB | 257.7 MB | 0.80 / 4.42 ms |
| Linux stable (Wine) | consumer | 1.11% | 1.07% | 2.27% | 40.5 MB | 274.7 MB | 36.6 MB | 265.8 MB | 0.29 / 3.06 ms |

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
