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

ADR 0021 removes `/websocket/v2/subscribe` in M4a. The selected-feed-only
`v2_subscription/*`, `v2_subscription_document/*` Criterion groups and
`subscription_cadence_cpu_and_wakeups` ignored benchmark are removed with that
protocol, not reported as performance regressions. Shared subscription wire
properties remain tested under `server::subscription_wire`; API v1 and legacy
filter benchmarks remain. M4a D17's bus-to-adapter cases replace measurements of
the removed transport rather than comparing unlike protocols.


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

### Interleaved revision comparison (Linux)

Build each revision first, with independent `CARGO_TARGET_DIR` directories and
identical features and populated fixtures. Then pass the already-built Criterion
executables, not Cargo commands, to the comparison tool:

```sh
sudo python3 scripts/bench-compare.py --cpu 7 --rounds 5 \
  --binary baseline=/absolute/baseline-target/release/deps/hot_paths-HASH \
  --binary candidate=/absolute/candidate-target/release/deps/hot_paths-HASH \
  --filter '^raw_snapshot_reads/stable_regular_fields$' \
  --output /absolute/new-comparison-directory
```

The tool reads Linux `thread_siblings_list` and reserves the **entire physical
core** using a temporary cgroup-v2 isolated cpuset partition. Only the benchmark
enters that partition, pinned to the requested logical CPU; its SMT siblings
are left idle. This needs sudo and an already-enabled root cpuset controller;
reservation failure aborts rather than silently falling back to taskset.
Benchmarks drop privileges to the invoking user. The empty partition is removed
on exit, returning its CPUs to the host. Kernel IRQs and shared CPU power/cache
effects are not isolated: stop builds and use a quiet window.

Runs interleave A,B,…,A,B,… for five rounds by default. Every invocation has a
fresh Criterion home, a saved log, and a bounded timeout (600 seconds by default).
`summary.json` reports each case's median of round medians, a deterministic
10,000-resample bootstrap 95% interval, raw round medians, and max–min spread.
Each candidate is compared with the first revision; a spread at least as large
as the absolute delta, or overlapping bootstrap intervals, is **inconclusive**.
Intervals describe these rounds, not immunity to systematic fixture or host
bias. Repeated `--binary` accepts more than two revisions. Timing controls:
`--warmup`, `--measurement`, `--samples`, `--rounds`, and `--timeout`.

## M5 D17 contract

A5 (A5.6, approved 2026-11-24) freezes these names, metrics and bars before
anything is measured. This is a contract. It is not measurement evidence and
does not claim that the cases exist yet. A result that fails a bar is fixed
and measured again; the bar is never moved after a result is seen.

**Brokered reads.** `benches/brokered_reads_bench.rs` keeps its custom
`harness = false` runner and the prototype plan shapes exactly:

- precise: 8 stages, 43 reads, 358 bytes;
- regular: 5 stages, 8 reads, 116 bytes.

Linux variants:

- The existing prototypes `direct`, `uds`, `shm` and `naive`, rerun in the
  same invocation.
- `production_sync`: the real M5 transport and core executor, with a real
  `vosu plugin-host` child submitting `VOSU_READ_PLAN_SYNC`.
- `production_async`: the same path without `SYNC`. It submits, receives
  `STATUS`, and has the result dispatched on the instance's dispatch thread.

Windows variants:

- `section`: the prototype protocol over an unnamed section and auto-reset
  events;
- `pipe`: the same protocol over a named pipe, for measurement only;
- `production_sync` and `production_async`.

Every variant runs the same dependent plans with identical bytes.

The runner prints `METRIC {shape}_{variant}_{metric}` for each of these
metrics:

- the existing `p50_ns`, `p99_ns`, `mean_ns`, `core_cpu_ns`, `plugin_cpu_ns`,
  `reads` and `bytes`;
- `round_trips`, which counts logical submissions per sample and must be 1
  for every brokered variant except `naive`;
- `wire_frames` (both directions, private frames included) and
  `control_frames` (private frames only);
- `wakeups`, `allocations`, `copies` and `clock_reads`;
- `result_sha256`, which must equal `direct`'s. A run with a mismatch is
  invalid, however fast it was.

Cold startup is reported separately as `startup_p50_ns` and `startup_p99_ns`,
measured from spawn to `READY`. The runner documents exactly which arguments it
supports, and Criterion flags are never passed to it. A missing variant is a
failure, not a skip.

**Bars.** Gated runs use approved exclusive pinned CPUs with child affinity
verified, five paired rounds and independent build and output directories.
Non-exclusive runs are exploratory only.

- **Linux.** For each shape, the median over five rounds of
  `production_sync p50 / shm p50`, both from the same invocation, is at most
  2.0. The historical prototype bars, 21.540 µs for precise and 13.126 µs for
  regular, are reported alongside and are never a substitute for the
  same-run comparison.
- **Async data.** `production_async` p50 and p99, with its acknowledgement
  frames counted, are required data. They are not gated, because the
  prototype has no dispatch-thread hop.
- **Windows.** The same 2.0 rule applies, with the selected transport's
  prototype variant as the denominator (`section`, or `pipe` if adopted).
- **Windows transport choice.** `pipe` replaces `section` only if all of the
  following hold:
  - its p50 is lower in all five paired rounds;
  - the median improvement is at least 10%;
  - the result hashes are equal;
  - a separately approved amendment provides a non-discoverable endpoint
    handoff (plugin-abi.md §7.4).

  Otherwise the unnamed section and events remain the production transport.

**Process-runtime osu! data.** These cases are required data, not a gate for
the bundled runtime:

- `plan_memory/stable_precise_process`, `plan_memory/lazer_precise_process`,
  `plan_memory/stable_regular_fields_process` and
  `plan_memory/lazer_regular_fields_process`. They use the same fixtures as the
  existing `_plan` cases, through a real plugin-host child.
- `live_reader_bench --memory process` emits `live_reader/process/stable/regular`,
  `live_reader/process/stable/precise`, `live_reader/process/lazer/regular`
  and `live_reader/process/lazer/precise`. These are capture-thread CPU and
  wall time per tick from the actual osu! producer in a process `cdylib`,
  not bundled CPU or process-wide CPU divided by an estimated poll rate.
- For each regular and precise capture report `submissions`, `stages`,
  `reads`, `read_bytes`, `wire_frames` and `control_frames`. Never
  extrapolate the read shapes from the prototype's 5-stage estimate.
- The exact profile options and output suffixes under `live_reader/process/`
  are `no_client` (attached, no downstream subscriber), `subscribed_precise`,
  `contention` (two process plugins using the same core broker),
  `delayed_dispatch`, `full_stream` and `idle_host` (started, no target
  offered). The idle case reports process CPU over an idle window, not
  CPU per nonexistent tick.

The generic broker bars above still apply to production transport code.

**Unchanged bundled paths.** The bundled reader, adapter and API paths keep
M4a's existing bar: the `state_bus_bench` cases `end_to_end/*` and
`adapter/*`, `memory_reader_bench`, and the existing `plan_memory` cases.
There must be no regression of more than 10% in the median, judged with
non-overlapping Criterion 95% intervals, per milestone and cumulatively
against pre-M0, with equivalent outputs. `performance_invariants` and
`runtime_wakeups` must keep passing.

Inherited findings stay open until their own evidence closes them, including
M1 PlanMemory's stable 2.65x result and the open CPU finding, and the open
findings from M2 through M4a.

```sh
cargo build --locked --release --example brokered_reads_target --example brokered_reads_plugin
cargo bench --locked --bench brokered_reads_bench
cargo bench --locked --bench plan_memory_bench -- 'plan_memory/.*_process$'
cargo bench --locked --bench memory_reader_bench
cargo bench --locked --bench state_bus_bench -- '^end_to_end/'
cargo test --locked --test performance_invariants --test runtime_wakeups
```

None of these commands was run by A5. Record results only from an approved
exclusive schedule.

### M5 P1 exploratory candidate result

These candidate runs shared the CPUs and VM; they do not decide D17. See
[brokered reads](../v2/brokered-reads.md#windows-transport-candidates-m5-p1-candidate-result)
for the candidate method, historical Windows rounds and contention record.

The `brokered_reads_bench` runner now runs on Linux and Windows:

- Linux runs `direct`, `uds`, `shm` and `naive`, plus `ring`, T0's ring over a
  memfd and an eventfd, as the reference.
- Windows runs `direct`, `section` (T0's ring over unnamed sections and
  auto-reset events) and `pipe` (a measurement-only named pipe).

Both candidate carriers use the same frames, policy, batching, handoff and no
flush, against real target memory. Each run is checked against the target's
own expected SHA-256 of the response. The runner takes `--bench`,
`--warmup`, `--samples`, `--startup-samples`, `--criterion-ms` and `--round`,
and nothing else.

```sh
# Linux, five rounds (round 1 through Cargo, rounds 2-5 the same executable)
cargo build --locked --release --example brokered_reads_target
taskset -c 0-7,16-23 cargo bench --locked --bench brokered_reads_bench -- --round 1
taskset -c 0-7,16-23 target/release/deps/brokered_reads_bench-HASH --bench --round N
```

```powershell
# Windows guest, after the same two builds with --no-default-features
cmd /c start "" /B /WAIT /AFFINITY FFFF target\release\deps\brokered_reads_bench-HASH.exe --bench --round N
```

Exploratory Linux medians of five rounds on `a63a0bb`, in µs:

| Platform | Shape | `direct` p50 | Candidate p50 (p99) | Reference p50 | Ratio |
|---|---|---:|---:|---:|---:|
| Linux | precise | 7.59 | `ring` 14.56 (41.06) | `shm` 11.25 | 1.297 |
| Linux | regular | 3.67 | `ring` 9.39 (20.33) | `shm` 6.85 | 1.383 |

### Windows official results (2026-11-26, `ad724666`)

Native Windows 11 Pro MSVC on the exclusive agent VM (16 vCPUs on the
9950X3D non-V-cache CCD), Rust 1.96.1, locked release/bench builds with
`--no-default-features` and the `win-x64` offset bundle. Exact source
`ad724666ab3212313bfd1f617fed73b6b524ad76` was staged by verified Git bundle;
the build and evidence trees were private to this revision. The target
and plugin hosts inherited `/AFFINITY FFFF` (CPUs 0–15). All five paired
rounds were accepted on attempt 1: the respective maximum
`load_other_busy_cpus` readings were 0.729, 0.521, 0.513, 0.614 and 0.415.
Raw logs, binary hashes, estimates and live receipts are in
`evidence/d17/windows-ad724666/` in the vecosystem evidence tree.

**Broker gate: PASS for both shapes.** Per the approved 2026-11-25 Windows
bar, the median of the five *paired* `production_sync p50 / section p50`
ratios must be at most 2.0. Brackets here and in the latency table are
95% percentile bootstrap intervals across the five round values.

| Shape | Paired ratio, median [95%] | Historical reference, µs (not gated) | Verdict |
|---|---:|---:|---|
| Precise | 1.738 [1.509, 1.768] | 167.2 | PASS |
| Regular | 1.817 [1.712, 2.028] | 129.7 | PASS |

Regular round 2 reached 2.028×; the frozen gate is on the median, not
each round or the upper confidence limit. The historical references are
2× the earlier quiet `section` p50 medians (83.6 and 64.9 µs).

| Shape | Variant | p50, µs [95%] | p99, µs [95%] |
|---|---|---:|---:|
| Precise | `direct` | 13.1 [12.9, 16.5] | 32.8 [31.3, 34.5] |
| Precise | `section` | 77.2 [72.6, 82.2] | 229.6 [183.0, 737.0] |
| Precise | `pipe` | 82.3 [76.4, 83.2] | 250.8 [232.6, 305.2] |
| Precise | `production_sync` | 129.3 [124.0, 136.5] | 883.1 [746.2, 1,230.7] |
| Precise | `production_async` | 300.4 [264.1, 323.8] | 1,774.0 [1,607.1, 2,085.4] |
| Regular | `direct` | 4.4 [4.3, 5.8] | 11.6 [10.8, 39.2] |
| Regular | `section` | 61.5 [59.9, 63.4] | 232.7 [205.8, 251.0] |
| Regular | `pipe` | 63.8 [60.4, 71.8] | 242.2 [212.0, 967.9] |
| Regular | `production_sync` | 111.0 [105.9, 122.9] | 625.7 [420.4, 1,904.2] |
| Regular | `production_async` | 202.3 [193.2, 213.0] | 1,457.2 [1,310.3, 2,059.2] |

Every variant matched `direct`'s result SHA-256 in all rounds with the
frozen 43/8 reads and 358/116 bytes. The brokered variants made one
submission each. The prototypes carried 2 wire / 0 control frames; both
production variants carried 4 wire / 2 control frames. Async is not a
performance gate. Median sync allocations per sample were 2.27 precise
and 1.21 regular, compared with 305.59 and 70.41 for async. Production
copy and clock-read counts were unmeasured; CPU, wakeups and Criterion
estimates are in the raw logs.

Cold spawn-to-`READY` p50/p99 in ms: `section` 16.1 [15.3, 20.0] /
22.2 [21.0, 31.7]; `pipe` 14.5 [14.1, 15.5] / 20.5 [19.1, 22.0];
production 25.1 [24.1, 26.0] / 44.4 [40.9, 56.9].
**Transport decision: keep the unnamed section and events.** Pipe won
only 1/5 precise and 2/5 regular paired p50 comparisons; its median paired
change was 7.5% and 3.5% slower respectively. Equal hashes alone do not
meet the pipe adoption rule.

**Process-runtime fixture data (not a gate).** All four `plan_memory`
cases matched the bundled plan digest before timing. Counts are per
capture over the 16-capture census; the brackets are Criterion median
95% intervals from the official fixture run.

| `plan_memory/` case | Median, µs [95%] | Submissions | Stages | Reads | Bytes | Wire | Control | Local scans |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `stable_precise_process` | 144.6 [136.7, 151.8] | 1 | 1 | 2 | 8 | 4 | 2 | 0 |
| `lazer_precise_process` | 1,269.2 [1,223.4, 1,309.4] | 8 | 17 | 51 | 1,840 | 32 | 16 | 0 |
| `stable_regular_fields_process` | 2,125.8 [1,990.7, 2,230.7] | 13 | 15 | 56 | 4,324 | 52 | 26 | 0 |
| `lazer_regular_fields_process` | 1,135.5 [1,111.3, 1,184.3] | 7 | 7 | 7 | 56 | 28 | 14 | 6 |

Lazer regular still uses the zero/not-ready fixture, not a populated
gameplay capture.

**Live process-runtime data (not a gate).** Stable `b20261118.1` and lazer
`2026.1117.0` ran as offline guests, one at a time in session 1. Each
accepted attached profile restarted its client's Autoplay replay of the
same 240-circle, 120 BPM synthetic `vosu D17 Replay` chart at the
beginning. The stable version is verified by the adapter reader; the
process-runtime receipt leaves that field blank. The attached rows used
the actual game PIDs, `--warmup 5 --cycles 20`, and inherited CPU affinity.
All ticks in the measured lanes were ready, with no capture errors.

Lane cells give mean capture-thread CPU ms / wall ms / million CPU cycles
per tick, with completed ticks in parentheses. These are one observed
window per profile, not confidence intervals. Zero CPU ms is possible
with Windows' coarse time accounting, even with nonzero executed cycles.
Ring columns are whole-window totals, including both lanes and shared
dispatch and publication traffic; the runner cannot split them by lane.

| Client | Profile | Regular CPU / wall / cycles (ticks) | Precise CPU / wall / cycles (ticks) | Wire / control frames |
|---|---|---:|---:|---:|
| Stable | `no_client` | 0.000 / 4.278 / 2.464 (20) | 0.872 / 3.741 / 2.140 (215) | 17,844 / 9,022 |
| Stable | `subscribed_precise` | 0.781 / 3.776 / 2.201 (20) | 0.300 / 2.779 / 1.906 (676) | 52,382 / 26,516 |
| Stable | `contention` | 0.000 / 4.062 / 2.344 (20) | 0.315 / 2.397 / 2.107 (745) | 57,594 / 29,162 |
| Stable | `delayed_dispatch` | 0.000 / 3.105 / 2.213 (20) | 0.432 / 2.301 / 1.839 (760) | 58,752 / 29,747 |
| Stable | `full_stream` | 0.000 / 3.396 / 2.144 (20) | 0.355 / 2.315 / 1.838 (748) | 57,874 / 29,302 |
| Lazer | `no_client` | 7.031 / 20.328 / 22.833 (20) | 0.289 / 2.233 / 1.219 (270) | 17,748 / 9,000 |
| Lazer | `subscribed_precise` | 2.344 / 15.149 / 9.330 (20) | 0.215 / 1.832 / 1.031 (943) | 43,341 / 22,026 |
| Lazer | `contention` | 0.000 / 13.786 / 9.390 (20) | 0.274 / 1.534 / 1.046 (1,025) | 46,459 / 23,609 |
| Lazer | `delayed_dispatch` | 5.469 / 24.820 / 18.625 (20) | 0.294 / 2.404 / 1.164 (850) | 39,903 / 20,286 |
| Lazer | `full_stream` | 1.562 / 16.296 / 17.865 (20) | 0.240 / 1.425 / 0.971 (1,041) | 46,977 / 23,867 |

Mean `submissions / stages / reads / bytes` per tick across these profiles:
stable regular 20–21 / 26–27 / 190–191 / 13,796–14,172;
stable precise 18.01–18.03 / 18.01–18.03 / 27.01–27.03 / 140.03–140.11;
lazer regular 88 / 92 / 236 / 9,231;
lazer precise 9.02–9.08 / 17.02–17.08 / 21.02–21.08 / 1,248.23–1,249.04.
Per-profile totals are in the receipts. The contender completed 28,213
stable-window and 22,480 lazer-window reads. Dispatch stayed held in
both requested profiles per client, and each full stream proved at
least 61,366,248 bytes queued (91.44% of the 64 MiB budget) and drained.
`idle_host` measured 0 reported process CPU ns over 5.015 s, with two
receipts from the same unattached child and zero capture ticks.

The temporary Defender exclusion, firewall rules, tasks and private
guest stage were removed after collecting the evidence. The Windows
result does not decide the separate Linux or bundled baseline
comparisons below.

### Linux official results (2026-11-27, `ad724666`)

**Method deviation (owner-approved):** the entire Linux measurement set ran
inside the pinned Arch Linux guest VM (`vosu-gaming.service`, kernel
`7.2.11-zen1-1-zen`, 4 physical cores / 8 vCPUs pinned 1:1 to host V-cache
CPUs `4-7,20-23`, host quiet), with both comparison arms in the same guest.
Rust 1.96.1, locked release/bench builds with default features
(`linux-process-events`) and the `linux-x64` offset bundle. Exact revisions
were staged from `ad724666.bundle` with independent target and Criterion
directories: candidate `ad724666`, M4a `49e95cf`, pre-M7 `33f957de`, and
pre-M0 `3a91ff95`. Raw logs, binary hashes, host/guest quiet snapshots and
receipts are in `evidence/d17/linux-ad724666/` in the vecosystem evidence
tree.

Two additional baseline-harness deviations are recorded:

- Exact M4a `49e95cf` could not compile or run `state_bus_bench` unmodified
  (`SchemaRegistry::register` E0599 in `benches/state_bus_bench.rs:83` and
  `action_schemas: &[]` in `benches/support/api_e2e.rs:27`). An untimed
  setup-only patch (`evidence/d17/linux-ad724666/m4a-baseline-setup.patch`)
  was applied only in the disposable `m4a` checkout; zero timed code changed.
- `per-m7` hit a pre-existing ephemeral-port bind collision in
  `benches/support/m7_e2e.rs:97` (`Hosts::start`) when back-to-back `m7_*`
  benchmarks ran in one invocation. The eight `per-m7` cases were run as four
  paired 5-round `bench-compare.py` invocations (`per-m7-bus-adapter`,
  `per-m7-idle`, `per-m7-dash`, `per-m7-tool`) using the exact same immutable
  binaries on isolated physical core `[6, 7]`.

**Invariants: PASS.** `performance_invariants` passed 20/20 and
`runtime_wakeups` passed 4/4 (1 manual test ignored). All broker correctness
hashes and `--test` case checks passed before timing.

**Broker gate: FAIL for both shapes in the pinned VM.** All five rounds ran
on guest CPUs `0-7` and were accepted on attempt 1 (maximum
`load_other_busy_cpus`: 1.528, 1.431, 1.479, 1.386, 1.445). Brackets are 95%
exact empirical bootstrap intervals across the five rounds.

| Shape | Paired ratio `production_sync / shm`, median [95%] | Historical reference, µs (not gated) | Verdict |
|---|---:|---:|---|
| Precise | 2.107 [1.857, 2.203] | 21.540 | **FAIL** |
| Regular | 2.385 [2.181, 2.462] | 13.126 | **FAIL** |

| Shape | Variant | p50, µs [95%] | p99, µs [95%] |
|---|---|---:|---:|
| Precise | `direct` | 4.25 [4.23, 4.28] | 14.61 [12.88, 19.81] |
| Precise | `uds` | 30.20 [29.79, 32.66] | 90.43 [83.66, 135.15] |
| Precise | `shm` | 23.16 [22.65, 26.84] | 86.78 [56.17, 115.58] |
| Precise | `naive` | 1,028.13 [1,021.52, 1,053.62] | 1,534.92 [1,366.30, 1,872.35] |
| Precise | `ring` | 29.29 [27.93, 30.98] | 95.29 [88.86, 122.98] |
| Precise | `production_sync` | 49.90 [48.80, 50.33] | 142.11 [138.58, 198.03] |
| Precise | `production_async` | 97.48 [96.25, 98.10] | 541.97 [229.39, 748.95] |
| Regular | `direct` | 1.65 [1.64, 1.74] | 2.98 [2.60, 3.92] |
| Regular | `uds` | 25.02 [24.79, 26.58] | 54.93 [48.15, 113.76] |
| Regular | `shm` | 18.88 [18.20, 20.44] | 73.38 [39.79, 86.18] |
| Regular | `naive` | 185.51 [184.83, 187.22] | 308.78 [303.54, 341.82] |
| Regular | `ring` | 22.49 [22.01, 23.04] | 52.32 [49.31, 79.36] |
| Regular | `production_sync` | 44.58 [43.69, 45.33] | 151.21 [126.94, 430.77] |
| Regular | `production_async` | 76.96 [75.20, 77.68] | 493.70 [188.87, 539.21] |

Every variant matched `direct`'s result SHA-256 in all rounds with the frozen
43/8 reads and 358/116 bytes, 1 round trip, 4 wire / 2 control frames for
both production variants, and median 1.03 allocations / 4.21 (precise) and
4.19 (regular) wakeups for `production_sync` (305.03 / 70.03 allocations and
9.41 / 9.29 wakeups for `production_async`). Cold spawn-to-`READY` p50/p99
in ms: `ring` 0.77 [0.72, 0.83] / 1.09 [1.00, 2.24]; production 3.08
[3.06, 3.12] / 4.89 [3.61, 6.19]. Note that KVM cross-vCPU wakeup overhead
inflated the 4-hop `production_sync` path more than 2-hop `shm` relative to
the bare-metal exploratory run (`1.46×`/`1.71×` at `201afe10`), but this gate
remains **FAIL / OPEN** and is not waived.

**Process-runtime fixture data (not a gate).** All four `plan_memory` cases
matched the bundled plan digest before timing (guest CPUs `0-7`, Criterion
median 95% intervals):

| `plan_memory/` case | Median, µs [95%] | Submissions | Stages | Reads | Bytes | Wire | Control | Local scans |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `stable_precise_process` | 42.92 [42.64, 43.13] | 1 | 1 | 2 | 8 | 4 | 2 | 0 |
| `lazer_precise_process` | 890.17 [883.97, 900.39] | 21 | 26 | 60 | 912 | 84 | 42 | 0 |
| `stable_regular_fields_process` | 566.62 [564.11, 574.03] | 13 | 15 | 56 | 4,324 | 52 | 26 | 0 |
| `lazer_regular_fields_process` | 336.58 [329.61, 344.93] | 7 | 7 | 7 | 56 | 28 | 14 | 6 |

**Live process-runtime data (Linux lazer `2026.1117.0`, not a gate).**
Private offline AppImage under headless Gamescope (CPUs `0-5`), Yama
`ptrace_scope = 1`, explicit `CAP_SYS_PTRACE` only on the reader launch,
pinned to CPU `6` (`affinity: [6]`), `--warmup 5 --cycles 20` on the same
synthetic `vosu D17 Replay` chart in Autoplay, 0 capture errors and all
measured ticks ready:

| Profile | Regular CPU / wall ms (ticks) | Precise CPU / wall ms (ticks) | Wire / control frames |
|---|---:|---:|---:|
| `no_client` | 0.255 / 0.892 (20) | 0.088 / 0.383 (289) | 32,791 / 16,522 |
| `subscribed_precise` | 0.215 / 0.836 (20) | 0.076 / 0.303 (2,179) | 215,643 / 108,170 |
| `contention` | 0.223 / 1.497 (20) | 0.073 / 0.531 (1,887) | 187,511 / 94,103 |
| `delayed_dispatch` | 0.212 / 0.812 (20) | 0.077 / 0.310 (2,185) | 216,126 / 108,414 |
| `full_stream` | 0.201 / 0.881 (20) | 0.073 / 0.301 (2,151) | 212,808 / 106,746 |

Mean `submissions / stages / reads / bytes` per tick: lazer regular
50–51 / 54–55 / 198–199 / 8,423–8,643; lazer precise 24.01–24.06 /
28.01–28.06 / 32.01–32.06 / 328.1–328.8. Contention completed 270,262
contender reads; dispatch stayed held in `delayed_dispatch` and `full_stream`;
`full_stream` queued at least 61,366,248 bytes (91.44% of the 64 MiB budget)
and drained. `idle_host` measured 95,449 ns process CPU over 5.000 s
(`affinity: [6]`, 2 receipts, 0 capture ticks).

**Bundled M5 comparisons (`memory_reader_bench`, `plan_memory/*`, `end_to_end/*`).**
Measured on isolated physical core `[6, 7]` (5 interleaved rounds).

- **`memory_reader_bench`: PASS / inconclusive.** `memory_reader_ns`
  10,628.7 ns (`-1.11%` vs `pre-m0`, `-0.94%` vs `m4a`, overlapping);
  `stable_idle_ns` 311.9 ns (`-8.74%` vs `pre-m0` within bar, `-0.34%` vs
  `m4a` overlapping); `stable_key_overlay_ns` 298.0 ns (`+3.29%` vs `pre-m0`
  within bar, `+0.02%` vs `m4a` overlapping).
- **`plan_memory/*` (`m4a` `49e95cf` → `ad724666`): PASS / inconclusive (0/11
  failures).** Significant speedups on `floor_eighteen_independent_ops`
  (`-14.88%`), `floor_fifty_six_independent_ops` (`-17.50%`), and
  `lazer_regular_fields` (`-18.00%`); the other eight cases had overlapping
  95% CIs (`-11.20%` to `+5.65%`).
- **`end_to_end/*` (`m4a` `49e95cf` → `ad724666`): FAIL (3/3 regressions >10%
  with non-overlapping 95% CIs).** All three were already present at pre-M7
  `33f957de`:

| Case | `m4a` `49e95cf`, µs [95%] | `ad724666`, µs [95%] | Delta | Verdict |
|---|---:|---:|---:|---|
| `end_to_end/bus` | 23.37 [23.28, 25.07] | 27.36 [26.92, 29.45] | +17.11% | **FAIL** |
| `end_to_end/bus_precise` | 16.52 [16.36, 18.70] | 19.30 [19.00, 20.33] | +16.84% | **FAIL** |
| `end_to_end/api_frame` | 14.24 [14.22, 14.94] | 16.83 [16.73, 19.95] | +18.21% | **FAIL** |

### M5 P1 production arm and X1 process-runtime data (exploratory)

The following Linux runs are exploratory and do not decide D17.
The production table below is the arm's first exploratory run, at `a5de65cb` plus
the chunked ring census of the commit after it (`cf17e6fc`); it missed the
bar. The [broker performance work](#broker-performance-work-exploratory-same-session-ab)
after it brought the same-session ratios under 2.0 in exploratory runs. All
of these are runs on the shared Linux host (Ryzen 9 9950X3D, Rust 1.96.1,
bench profile, default features), pinned
`taskset -c 0-7,16-23 nice -n 10` while other agents compiled. The first run
had a load average of 19–23, with `load_other_busy_cpus` at 2.4–6.8 busy
CPUs during the production sessions. Nothing ran on the Windows VM.

**What runs.** `brokered_reads_bench` gains `production_sync` and
`production_async` (`benches/support/brokered/production.rs`): this bench
process is core, a real `PluginHost` with the real process launcher, which
starts the real `vosu plugin-host` child of the `vosu` executable Cargo built
for the bench (`CARGO_BIN_EXE_vosu`). The child loads the bench plugin
(`examples/brokered_reads_plugin.rs`), which submits the frozen plans through
the SDK (`PlanMemory::read_flat` for `SYNC`, `send_flat` then the dispatched
result for async), the relay, ring, ingress, broker workers and executor
against the real target. The only bench seams are `bench-harness` (never
production): the launcher without an approval record and, for X1, the
session-memory hook. An ABI stage has one base, so precise's per-key stage
(18 bases) is 18 ABI stages: 25 ABI stages, same 43 reads, 358 bytes, order,
response hash and one submission. The diagnostic `executor` variant (not
D17) runs core's `execute_sync_into` on the same ABI plan in process.

The harness fails on a result hash other than the target's and `direct`'s,
any response mismatch, a sample that is not one plan frame and one result
frame (counted from the child's own rings), a plugin-host affinity other
than core's (its own report and core's read of its pid), a zero plugin-host
CPU figure, or a lost plugin host. Production code counts no copies or clock
reads, so those two metrics print `unmeasured`; plugin allocations count the
plugin library only, not the host relay.

| Shape | `direct` | `shm` | `ring` | `executor` | `production_sync` p50 (p99) | ratio to `shm` | historical bar | `production_async` p50 (p99) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| precise | 7.42 | 11.46 | 14.22 | 17.17 | 30.96 (141.3) | **2.70** | 21.54 | 51.91 (256.5) |
| regular | 3.55 | 6.64 | 9.42 | 3.53 | 15.01 (73.9) | **2.26** | 13.13 | 23.96 (441.7) |

(p50 in µs.) Per round trip, both production variants carried 4 wire
frames, 2 of them private control frames (the prototypes carry 2 and 0).
`production_sync` made about 5 context switches (`ring` 1.96), 3.03
allocations (all in core) and used 28.7/12.6 µs core and 7.7/7.5 µs
plugin-host CPU (precise/regular). Cold start, `create_instance` to
`READY`: 3.3 ms p50, 4.8 ms p99. Attribution, an inference from these
figures and not a profile: for precise, the 25-stage executor alone costs
17.2 µs against `direct`'s 7.4 µs (25 native batches instead of 8); for
regular, the executor costs what `direct` does, so about 11.5 µs is the
process boundary (relay, ingress, broker-worker hop, control frames and
wakeups), against 3.1 µs for the prototype `shm`.

**X1 per-capture counts.** `plan_memory_bench` gains
`plan_memory/{stable_precise,lazer_precise,stable_regular_fields,lazer_regular_fields}_process`
(`benches/support/plan_process.rs`): the `_plan` cases' fixtures and reader
calls (`benches/support/plan_process_cases.rs`), with the reader in a real
plugin host decoding through `vosu_osu::memory::PlanProcessMemory`. Each
case must decode to its `_plan` case's digest before timing. Counted per
capture over 16 captures, identical in every capture:

| Capture | Submissions | Stages | Reads | Read bytes | Wire frames | Control frames | Local scans |
|---|---:|---:|---:|---:|---:|---:|---:|
| stable precise | 1 | 1 | 2 | 8 | 4 | 2 | 0 |
| lazer precise | 21 | 26 | 60 | 912 | 84 | 42 | 0 |
| stable regular fields | 13 | 15 | 56 | 4,324 | 52 | 26 | 0 |
| lazer regular fields (zero fixture) | 7 | 7 | 7 | 56 | 28 | 14 | 6 |

A whole capture is therefore many round trips, not one. Lazer regular uses
the existing zero-filled fixture (no populated lazer regular fixture exists),
so it measures the not-ready path and its region-walk scans. A short
exploratory Criterion pass (1 s warm-up, 2 s measurement) gave 10.5 µs,
212 µs, 137 µs and 86 µs. `plan_memory_bench`'s native handles now scan
through core's region walk as the plan handles do; since M5 R1 its native
lazer assertion had failed before any case ran.

The live runner (commands below) reports the actual osu! `cdylib`'s regular
and precise capture-thread CPU and wall time per completed tick. Raw workload
totals include ready and error counts and the plans' requested stages, reads
and bytes; the four `plan_memory` cases above census actual wire/control
frames in isolated per-capture windows. The live profile reports its actual
aggregate ring frames (including control traffic); live dispatch and
publication share those rings, so these totals are not attributed to
individual capture threads. On Windows, also report executed CPU cycles per
tick because `GetThreadTimes` can round small charges down to zero.

#### Broker performance work (exploratory, same-session A/B)

These numbers come from the shared host under the same pinning as above,
with load averages of 6–24 and up to 10.5 other CPUs busy in some rounds.
They are exploratory and are not D17 evidence. Each A/B runs the two
builds' bench executables in interleaved rounds (ABAB…). Each build has its
own target directory and Criterion home. The figure given is the median of
the round medians.

**Where the time went at `cf17e6fc`.** `perf`, `strace` and root are
unavailable on this host, so two other methods were used, and neither was
committed:

- Per-thread context switches were sampled from `/proc/<pid>/task/*`. Each
  `SYNC` round trip woke five threads: core's ring reader, its `SYNC` budget
  timer and a broker worker, then the child's ring reader and the plugin
  thread.
- Timestamp probes at every hop were joined by request id across both
  processes, giving this precise-plan breakdown (p50, 29.8 µs in total):

| Hop | p50 (µs) |
|---|---:|
| Plugin sends `ACK_NEXT` + submit and rings the doorbell | 1.17 |
| Core's ring reader wakes | 1.65 |
| Reader reads the submission | 0.45 |
| Reader hand-off: notifies the budget timer, queues the broker job, 2 allocations (p90 21 µs) | 1.36 |
| Broker worker wakes | about 1.6 |
| **Broker: validate, execute, dispatch** | **17.8** |
| Send `REPLY_NEXT` + result and ring | 1.29 |
| Child's ring reader wakes | 2.05 |
| Hand the result to the plugin thread | 0.57 |
| Plugin thread wakes | 0.88 |

The executor was the largest cost, because each of the 25 ABI stages was
one `process_vm_readv`. The budget-timer wake added a futex wake to the
reader's critical path, ahead of the broker hand-off.

**Changes and their A/B runs.** Each change is a separate commit, measured
over four interleaved rounds against the commit before it:

| Commit | Change | Against | Result |
|---|---|---|---|
| `3f8c1d9b` | The executor reads each dependency level in one `read_many` batch (stack batches of 64). Precise goes from 25 native calls to 6, regular from 5 to 2. A failed batch falls back to per-op reads. The `SYNC` deadline is checked between levels. | `cf17e6fc` | `executor` p50 17.14 → 5.95 µs (precise) and 3.46 → 1.76 µs (regular). `production_sync` p50 30.32 → 18.52 µs and 14.89 → 12.87 µs. Ratio to `shm` 2.77 → 1.67 and 2.25 → 1.90. |
| `a0c94fa6` | The reader wakes the budget timer only when a deadline precedes the one it is parked on, about once per 2 s budget. Pending deadlines are a sorted `Vec`, the broker job is boxed once, and emptied pool sub-queues are reused. | `3f8c1d9b` | Core wakeups per round trip 3.04 → 2.02. Core allocations 3.04 → 1.04. Regular core CPU 9.95 → 8.62 µs. Regular p50 12.87 → 11.98 µs, ratio 1.90 → 1.81. Precise ratio 1.67 → 1.59; its p50 median rose 18.52 → 19.58 µs because one round ran with 10.5 other CPUs busy. |
| `201afe10` | The relay validates a `SYNC` plan flat (`ValidatedPlan` plus the module name's UTF-8 check) instead of decoding it into an owned `ReadPlanSubmit`. Statuses are unchanged. | `a0c94fa6` | Precise p50 17.35 → 16.87 µs, lower in every pair, ratio 1.59 → 1.46. Regular unchanged within noise: 11.55 → 11.63 µs, ratio 1.76 → 1.71. |
| `be8d0e2d` | A level of one read uses `read_exact` directly, single-stage plans skip level scheduling, and the failure paths move out of line, to keep the bundled path from regressing. | `cf17e6fc` | Bundled `plan_memory` only; see the table below. The production arm was not re-measured at this commit. |

**Final exploratory ratios.** The three measurements of changes 1–2 or 1–3
gave a `production_sync` p50 over the same run's `shm` of **1.46–1.59**
(precise) and **1.71–1.81** (regular). The last one (`201afe10`, load
average 7–11) gave precise 16.87 µs against `shm` 11.52 µs, and regular
11.63 µs against 6.98 µs. `executor` was 5.89 and 1.78 µs. A round trip still
costs about 2 core wakeups, 2 plugin wakeups and 1.04 core allocations (the
broker job's `Box`). Core CPU was 13.4 and 8.3 µs. These ratios come from a
loaded, shared host and are not the D17 result. D17 is decided only by the
official sequence below, on an exclusive host.

**The remaining gap is four hops, and the contract fixes them.** About 8 µs
of each round trip is four consecutive thread hand-offs: plugin thread →
core's ring reader → broker worker → child's ring reader → plugin thread.
The prototype `ring` has two. Each hop is required by the approved ABI §7.6
([plugin-abi.md](../v2/plugin-abi.md#76-process-supervision-protocol-a5-approved-2026-11-24)):

- core's ring reader never handles a message or writes a reply itself;
- each `SYNC` plan is queued on core's bounded broker workers (M5 B0);
- the child's waiting thread, not its ring reader, dispatches a `SYNC`
  result.

Removing any of these hops is a contract change, so none was made.

**Bundled `plan_memory`, `be8d0e2d` against `cf17e6fc`.** Four interleaved
rounds of the bundled cases, load average 5–20, ratio head/base:

| Case | `cf17e6fc` | `be8d0e2d` | Ratio |
|---|---:|---:|---:|
| `floor_one_op` | 0.058 µs | 0.061 µs | **1.055** |
| `floor_eighteen_independent_ops` | 0.254 µs | 0.205 µs | 0.805 |
| `floor_fifty_six_independent_ops` | 0.689 µs | 0.551 µs | 0.800 |
| `stable_precise_native` | 0.009 µs | 0.009 µs | 0.993 |
| `stable_precise_plan` | 0.083 µs | 0.077 µs | 0.924 |
| `stable_native` | 1.301 µs | 1.238 µs | 0.952 |
| `stable_unfused_plans` | 2.847 µs | 2.614 µs | 0.918 |
| `stable_regular_fields` | 2.849 µs | 2.715 µs | 0.953 |
| `lazer_precise_native` | 0.515 µs | 0.510 µs | 0.991 |
| `lazer_precise_plan` | 2.233 µs | 2.282 µs | 1.022 |
| `lazer_regular_fields` | 11.772 µs | 11.957 µs | 1.016 |

`floor_one_op` is 3 ns (5.5%) slower. That is a small regression, still
open, though within D17's bundled bar of no regression over 10%. The
`lazer_precise_plan` and `lazer_regular_fields` ratios of 1.022 and 1.016 are
within the run-to-run spread: the session before measured the same two
cases at 1.001 and 0.977. Before change 4, changes 1–3 measured
`floor_one_op` at 1.155, `stable_precise_plan` at 1.184 and
`stable_regular_fields` at 1.094 against `cf17e6fc` (three rounds, load
average 8–15). That run is why change 4 exists.

#### Official command sequence (Main, exclusive host and VM)

Linux, one checkout per revision, each with its own target and Criterion
home, offset bundle prepared (`python scripts/prepare-offset-generator.py
--rid linux-x64`), `source scripts/dev-env.sh`, `RUSTC_WRAPPER=sccache`:

```sh
# Changed head
export CARGO_TARGET_DIR=$ROOT/targets/m5 CRITERION_HOME=$ROOT/criterion/m5
cargo build --locked --release --example brokered_reads_target --example brokered_reads_plugin
cargo bench --locked --bench brokered_reads_bench --bench plan_memory_bench \
  --bench memory_reader_bench --bench state_bus_bench --no-run
# M4a head 49e95cf (implementation-plan.md "M4a outcome")
export CARGO_TARGET_DIR=$ROOT/targets/m4a CRITERION_HOME=$ROOT/criterion/m4a
cargo bench --locked --bench plan_memory_bench --bench memory_reader_bench --bench state_bus_bench --no-run
# pre-M0 3a91ff9 (only memory_reader_bench and hot_paths exist there)
export CARGO_TARGET_DIR=$ROOT/targets/pre-m0 CRITERION_HOME=$ROOT/criterion/pre-m0
cargo bench --locked --bench memory_reader_bench --no-run

# Changed head, pinned
taskset -c 0-7,16-23 cargo test --locked --test performance_invariants --test runtime_wakeups
for N in 1 2 3 4 5; do   # gate: per shape, median of production_sync_p50_over_reference <= 2.0
  taskset -c 0-7,16-23 $ROOT/targets/m5/release/deps/brokered_reads_bench-HASH --bench --round $N
done
taskset -c 0-7,16-23 cargo bench --locked --bench plan_memory_bench -- 'plan_memory/.*_process$'
# Bundled paths, interleaved, five rounds, one reserved core
sudo python3 scripts/bench-compare.py --cpu 7 --rounds 5 \
  --binary m4a=$ROOT/targets/m4a/release/deps/plan_memory_bench-HASH \
  --binary m5=$ROOT/targets/m5/release/deps/plan_memory_bench-HASH \
  --filter '^plan_memory/(floor_one_op|floor_eighteen_independent_ops|floor_fifty_six_independent_ops|stable_precise_native|stable_precise_plan|stable_native|stable_unfused_plans|lazer_precise_native|lazer_precise_plan|stable_regular_fields|lazer_regular_fields)$' \
  --output $ROOT/evidence/plan-memory
sudo python3 scripts/bench-compare.py --cpu 7 --rounds 5 \
  --binary m4a=$ROOT/targets/m4a/release/deps/state_bus_bench-HASH \
  --binary m5=$ROOT/targets/m5/release/deps/state_bus_bench-HASH \
  --filter '^end_to_end/(bus|bus_precise|api_frame)$' --output $ROOT/evidence/end-to-end
# memory_reader_bench is a METRIC runner, not Criterion: alternate the three
# executables five times on the same pinned core and compare METRIC medians.
for N in 1 2 3 4 5; do for R in pre-m0 m4a m5; do
  taskset -c 7 $ROOT/targets/$R/release/deps/memory_reader_bench-HASH --bench
done; done
```

`end_to_end/adapter*` did not exist at `49e95cf`, and no pre-M0 equivalent of
the state-bus or `plan_memory` cases exists at this head, so those
comparisons need a common legacy harness first.

Windows guest, exclusive VM, same source, `--rid win-x64` bundle; prime the
fresh `vosu.exe` once (`target\release\vosu.exe plugin-host`, exit 70):

```powershell
cargo build --locked --release --no-default-features --example brokered_reads_target --example brokered_reads_plugin
cargo bench --locked --no-default-features --bench brokered_reads_bench --bench plan_memory_bench --no-run
foreach ($n in 1..5) {   # gate: median of production_sync_p50_over_reference (over section) <= 2.0
  cmd /c start "" /B /WAIT /AFFINITY FFFF target\release\deps\brokered_reads_bench-HASH.exe --bench --round $n
}
cmd /c start "" /B /WAIT /AFFINITY FFFF target\release\deps\plan_memory_bench-HASH.exe --bench "plan_memory/.*_process$"
```

Rerun any round whose `load_other_busy_cpus` reached 2 or more.

#### Official live process-runtime commands (Main)

Use the same release checkout and quiet window as above. Keep the selected
client in repeatable gameplay (the same version, chart, and replay section
for every profile); record any version, state or input changes. Do not use
`--process-fixture`: that uses a native child with a synthetic stable graph
and resolved signature anchors, solely to check the harness without a game.
The live path uses the actual game's discovery facts, brokered scan and memory
reads, capture workers and state publication. Core verifies the local package
through the normal approval gate using a fresh, private approval store; the
benchmark process never loads the `cdylib` itself. Keep the real target readable
to the benchmark core under the OS's normal process permissions.

For a short path check only, replace `--pid PID --client CLIENT --warmup 5
--cycles 20` with `--process-fixture --warmup 1 --cycles 2` in each attached
profile. Use `--cycles 1` for a 100 ms `idle_host` smoke. A smoke must show
ready regular and precise work and broker traffic for every attached
profile, plus proof of live demand, contention, dispatch hold and delivery
pressure in the corresponding profiles. Idle must retain the same child,
with two control receipts and zero capture ticks.

To smoke the four fixture-based cases without timing, build
`--example brokered_reads_plugin` in release and run
`cargo bench --locked --bench plan_memory_bench -- 'plan_memory/.*_process$' --test`.
This still checks the captured digests and prints the nonzero census.

`subscribed_precise` keeps a real surface subscription to `vosu.osu/beatmap/time/live`.
The same subscription is active in `contention`, `delayed_dispatch` and
`full_stream`. Contention attaches a second child plugin to its own native
memory target on the same core broker. Delayed dispatch holds one scoped
callback while capture workers continue. The full-stream case queues legal
near-1 MiB messages on that scope, proves at least 90% of the child delivery
budget was occupied during the sample, then verifies the bytes drained.
These are transport pressure controls, not game-generated actions. Idle host
measures five seconds between two control receipts, with no attached session
or capture ticks. Report idle CPU even if the native CPU clock rounds it to
zero. The live data are not a pass/fail performance bar.

Linux (lazer; also run stable under Wine when available). Set `LAZER_PID` and,
if running stable, `STABLE_PID` to the exact native process IDs. Use the CPU
approved and reserved for this window (7 below); the runner checks that the
plugin host inherits the chosen mask.

```sh
export CARGO_TARGET_DIR="$ROOT/targets/m5"
cargo build --locked --release --features linux-process-events \
  --bin vosu --example live_reader_bench --example live_reader_process_plugin
mkdir -p "$ROOT/evidence/live-reader"
reader="$CARGO_TARGET_DIR/release/examples/live_reader_bench"
"$reader" --list-process-cases > "$ROOT/evidence/live-reader/linux-cases.txt"
for client in lazer stable; do
  case "$client" in lazer) pid="$LAZER_PID";; stable) pid="$STABLE_PID";; esac
  if [ "$client" = stable ] && [ -z "$pid" ]; then continue; fi
  for profile in no_client subscribed_precise contention delayed_dispatch full_stream; do
    taskset -c 7 "$reader" --memory process --pid "$pid" --client "$client" \
      --profile "$profile" --warmup 5 --cycles 20 \
      > "$ROOT/evidence/live-reader/linux-$client-$profile.json" || exit 1
  done
done
taskset -c 7 "$reader" --memory process --profile idle_host \
  > "$ROOT/evidence/live-reader/linux-idle_host.json"
```

Native Windows guest (not cross-built); set `$stablePid`, `$lazerPid` and `$root`
before running. Use the guest's approved affinity mask (`FFFF` below).

```powershell
$env:CARGO_TARGET_DIR = Join-Path $root 'targets\m5'
cargo build --locked --release --no-default-features --bin vosu --example live_reader_bench --example live_reader_process_plugin
if ($LASTEXITCODE -ne 0) { throw 'Live reader build failed' }
$release = Join-Path $env:CARGO_TARGET_DIR 'release'
& "$release\vosu.exe" plugin-host           # prime this image; expected exit 70
if ($LASTEXITCODE -ne 70) { throw 'Unexpected plugin-host preflight result' }
$reader = Join-Path $release 'examples\live_reader_bench.exe'
$evidence = New-Item -ItemType Directory -Force (Join-Path $root 'evidence\live-reader')
& $reader --list-process-cases | Set-Content "$evidence\windows-cases.txt"
foreach ($client in 'stable','lazer') {
  $targetPid = if ($client -eq 'stable') { $stablePid } else { $lazerPid }
  foreach ($profile in 'no_client','subscribed_precise','contention','delayed_dispatch','full_stream') {
    cmd /c start "" /B /WAIT /AFFINITY FFFF "$reader" --memory process --pid $targetPid --client $client --profile $profile --warmup 5 --cycles 20 |
      Set-Content "$evidence\windows-$client-$profile.json"
    if ($LASTEXITCODE -ne 0) { throw "$client $profile failed" }
  }
}
cmd /c start "" /B /WAIT /AFFINITY FFFF "$reader" --memory process --profile idle_host |
  Set-Content "$evidence\windows-idle_host.json"
if ($LASTEXITCODE -ne 0) { throw 'idle_host failed' }
```

## M7 D17 contract

M7 X1 freezes these additional `state_bus_bench` cases before timing:

- `end_to_end/m7_idle_hosts`: alternating, nonzero source captures committed
  through the ABI and state bus with the dashboard, overlay and tool hosts
  composed. No HTTP or WebSocket clients during the sample.
- `end_to_end/m7_dashboard_subscription`: those same source changes through
  the regular public `/vosu.play/score` selector over a real API v1 WebSocket
  on the overlay listener, with the dashboard listener as its `Origin`.
  This models an active dashboard-origin subscriber; the shipped status
  strip itself makes one-shot reads and never opens a subscription.
- `end_to_end/m7_tool_action_progress`: a launched web tool starts an ABI
  action, reads nonzero progress through the real job API, cancels and
  observes the terminal result. The launch and single-use exchange are setup,
  not timed work. Each iteration starts a fresh job.

Also retain `end_to_end/{bus,bus_precise,api_frame,adapter,adapter_precise}`.
The quality bar is unchanged: no median regression above 10% with
non-overlapping 95% intervals, per milestone and cumulatively on comparable
outputs. A missing protocol or baseline harness is not a pass.

The idle and dashboard cases publish scores 12,345/23,456 into the
production host through the same fixture producer. An idle iteration must
advance the bus revision; a dashboard iteration must deliver the new score
in a distinct delta. The tool case must return fraction `0.375` at revision
1 and the job's terminal `cancelled` at revision 2. The benchmarks exclude
startup and teardown and use no real game, browser, or external server.
`runtime_wakeups` checks that the composed hosts start no idle state
delivery or extra sampling cadence, that the subscription releases demand
on teardown, and that neither workload fills the legacy adapter caches.
The pre-existing tool-directory rescan and process discovery reconciliation
are housekeeping, not state polling; they remain enabled.

### Official comparison (Main, exclusive Linux window)

Use the frozen pre-M7 head `33f957de` (M7 A7's recorded pre-extraction
baseline, also the last first-parent revision before M7), and pre-M0
`3a91ff95`. The pre-M7 setup script adds the same three workload clients
to that revision's benchmark, injecting only temporary configuration,
broker path, and process discovery into the legacy production composition.
Its route handlers, tool authorization, providers and job API are unmodified.
The inherited five cases are already present at `33f957de`.

The legacy v2 and precise serializers have the same stable, empty-history
source shape in both `hot_paths` targets. Their cold output cost is the
cumulative pre-M0 comparison below; it is **not** a source-to-frame delta.
Pre-M0 is **not applicable** for the three M7 host cases or the full
`end_to_end` cases: it has no comparable state bus, API v1 frames or
launch-authorized action/job protocol. Never compare those cases to a
different output just to produce a cumulative number.

In Bash, set `REPO` to the registered source repo (not an ovnfs checkout)
and `M7` to the clean candidate checkout containing this harness. Use
disposable baseline checkouts and a new evidence directory:

```bash
set -euo pipefail
: "${REPO:?absolute source repository}" "${M7:?absolute candidate checkout}"
ROOT=$(mktemp -d "$HOME/vosu-m7-d17.XXXXXXXX")
PRE_M7=$(cd "$REPO" && ovnfs new --name d17-m7-pre-m7 --rev 33f957de)
PRE_M0=$(cd "$REPO" && ovnfs new --name d17-m7-pre-m0 --rev 3a91ff95)
python3 "$M7/scripts/prepare-m7-bench-baseline.py" "$PRE_M7"

build() (
  cd "$1"
  local label=$2 bench
  shift 2
  if [[ -f scripts/dev-env.sh ]]; then source scripts/dev-env.sh; fi
  export PATH="$HOME/.cargo/bin:$PATH" RUSTC_WRAPPER=sccache
  export CARGO_TARGET_DIR="$ROOT/targets/$label" CRITERION_HOME="$ROOT/criterion/$label"
  taskset -c 0-7,16-23 nice -n 10 python3 scripts/prepare-offset-generator.py --rid linux-x64
  for bench in "$@"; do
    taskset -c 0-7,16-23 nice -n 10 cargo bench --locked --no-run \
      --bench "$bench" --message-format=json >"$ROOT/$label-$bench.jsonl"
    jq -sre --arg bench "$bench" '
      [.[] | select(.reason == "compiler-artifact" and .target.name == $bench
                   and (.target.kind | index("bench"))) | .executable | strings]
      | unique | if length == 1 then .[0] else error("expected one bench executable") end
    ' "$ROOT/$label-$bench.jsonl" >"$ROOT/$label-$bench.bin"
  done
)
build "$M7" m7 state_bus_bench hot_paths
build "$PRE_M7" pre-m7 state_bus_bench
build "$PRE_M0" pre-m0 hot_paths

E2E='^end_to_end/(bus|bus_precise|api_frame|adapter|adapter_precise|m7_idle_hosts|m7_dashboard_subscription|m7_tool_action_progress)$'
COLD='^serializers_cold/(v2|v2_precise)$'
for rev in pre-m7 m7; do
  CRITERION_HOME="$ROOT/criterion/$rev" \
    "$(cat "$ROOT/$rev-state_bus_bench.bin")" --test "$E2E"
done
for rev in pre-m0 m7; do
  CRITERION_HOME="$ROOT/criterion/$rev" \
    "$(cat "$ROOT/$rev-hot_paths.bin")" --test "$COLD"
done
# All 8 end-to-end and 2 cold-output cases must execute and pass the checks above.
# Stop builds and other performance work before starting the exclusive runs.
sudo python3 "$M7/scripts/bench-compare.py" --cpu 7 --rounds 5 \
  --binary "pre-m7=$(cat "$ROOT/pre-m7-state_bus_bench.bin")" \
  --binary "m7=$(cat "$ROOT/m7-state_bus_bench.bin")" \
  --filter "$E2E" --output "$ROOT/per-m7"
sudo python3 "$M7/scripts/bench-compare.py" --cpu 7 --rounds 5 \
  --binary "pre-m0=$(cat "$ROOT/pre-m0-hot_paths.bin")" \
  --binary "m7=$(cat "$ROOT/m7-hot_paths.bin")" \
  --filter "$COLD" --output "$ROOT/cumulative-pre-m0"
```

The comparison tool assigns a fresh Criterion home per revision **and**
round under each output directory. Save the source revisions, SHA-256s of
the binaries, the test-mode case lists, environment, and both
`summary.json` files. A short benchmark smoke proves nonzero work only;
it does not close D17.

### M7 Linux official results (2026-11-27, `ad724666`)

Measured in the same pinned Linux VM window (`vosu-gaming.service`, isolated
physical core `[6, 7]`, 5 interleaved rounds per comparison, independent
target/Criterion directories; receipts in `evidence/d17/linux-ad724666/`).
**Verdict: FAIL / OPEN** — two per-M7 `end_to_end/*` cases, three
inherited `adapter/*` cases (against pre-M7 `33f957de`, the first available
like-for-like baseline; the M3/pre-M0 adapter comparator remains
unestablished), and cumulative `serializers_cold/v2` exceeded the +10%
non-overlapping-CI bar.

**Per-M7 `end_to_end/*` (`33f957de` → `ad724666`):**

| Case | `pre-m7` `33f957de`, µs [95%] | `ad724666`, µs [95%] | Delta | Verdict |
|---|---:|---:|---:|---|
| `end_to_end/adapter` | 114.37 [113.66, 119.98] | 123.66 [122.68, 125.02] | +8.12% | within bar |
| `end_to_end/adapter_precise` | 37.47 [37.29, 37.76] | 41.75 [39.77, 42.04] | +11.42% | **FAIL** |
| `end_to_end/api_frame` | 16.71 [16.68, 17.53] | 16.69 [16.66, 16.85] | -0.15% | inconclusive |
| `end_to_end/bus` | 28.75 [27.34, 29.05] | 27.37 [27.25, 28.88] | -4.80% | inconclusive |
| `end_to_end/bus_precise` | 19.72 [18.85, 20.50] | 18.92 [18.80, 18.97] | -4.06% | inconclusive |
| `end_to_end/m7_dashboard_subscription` | 2,072.72 [2,056.66, 2,084.52] | 2,066.57 [2,042.89, 2,092.09] | -0.30% | inconclusive |
| `end_to_end/m7_idle_hosts` | 1.53 [1.52, 1.76] | 1.59 [1.56, 1.81] | +4.15% | inconclusive |
| `end_to_end/m7_tool_action_progress` | 214.22 [212.80, 216.47] | 244.16 [242.90, 247.94] | +13.98% | **FAIL** |

**Cumulative cold serializers (`pre-m0` `3a91ff95` → `ad724666`):**

| Case | `pre-m0` `3a91ff95`, µs [95%] | `ad724666`, µs [95%] | Delta | Verdict |
|---|---:|---:|---:|---|
| `serializers_cold/v2` | 2.57 [2.54, 2.67] | 8.47 [8.44, 8.50] | +229.32% | **FAIL** |
| `serializers_cold/v2_precise` | 0.96 [0.95, 1.10] | 0.23 [0.22, 0.24] | -76.02% | within bar |

**Inherited `adapter/*` (`pre-m7` `33f957de` → `ad724666`):**

| Case | `pre-m7` `33f957de`, µs [95%] | `ad724666`, µs [95%] | Delta | Verdict |
|---|---:|---:|---:|---|
| `adapter/filtered_clients` | 1,911.08 [1,902.89, 2,033.94] | 2,081.00 [2,057.30, 2,086.22] | +8.89% | within bar |
| `adapter/focus_switch` | 414.98 [406.94, 482.09] | 457.36 [442.22, 475.01] | +10.21% | inconclusive |
| `adapter/http_then_ws_cache` | 314.24 [310.18, 329.04] | 348.73 [344.30, 370.23] | +10.98% | **FAIL** |
| `adapter/idle_no_clients` | 10.46 [10.41, 11.07] | 10.50 [10.43, 10.88] | +0.36% | inconclusive |
| `adapter/plain_100_clients` | 9,550.78 [9,492.73, 9,997.01] | 10,791.26 [10,703.10, 11,016.25] | +12.99% | **FAIL** |
| `adapter/sc_changed_chart` | 647.44 [645.94, 651.49] | 649.40 [635.82, 655.70] | +0.30% | inconclusive |
| `adapter/sc_unchanged_chart` | 206.08 [204.52, 218.13] | 206.73 [195.02, 243.12] | +0.32% | inconclusive |
| `adapter/slow_consumer` | 1,047.82 [1,042.83, 1,108.33] | 1,158.37 [1,154.24, 1,347.69] | +10.55% | **FAIL** (spread > delta) |

## M4a D17 contract

A4 freezes the following exact future case names in `state_bus_bench`; this
is a contract, not measurement evidence or a claim those cases exist yet:

- `end_to_end/adapter`: source snapshot → real osu! projection → ABI batch →
  bus commit → coherent surface → actual `/json/v2` bytes **and encoded API-v1
  subscription frame**, including delivery/write acknowledgement;
- `end_to_end/adapter_precise`: the same production path for a precise-only
  change and precise frame, retaining the unchanged regular view;
- equivalent `end_to_end/legacy` output work from M3 and pre-M0 serializers;
- `adapter/idle_no_clients`, `adapter/http_then_ws_cache`,
  `adapter/plain_100_clients`, `adapter/filtered_clients`,
  `adapter/slow_consumer`, `adapter/focus_switch`,
  `adapter/sc_unchanged_chart`, `adapter/sc_changed_chart`.

List actual matched names/nonzero work counts before timing. New adapter case
names require a common harness around both implementations; never pass a
nonexistent name to a baseline binary. Cold chart I/O/parser cost is reported
separately. Preserve nonzero distinctive input and equivalent actual output
work, not a prebuilt DTO or publication-only comparison. Production counters
prove shared HTTP/WS bytes, one in-flight/latest-only frame, bounded filters,
zero no-client work, regular-cache retention and unchanged-chart parse count.
The bounds and instance-owned hooks are in
[adapter surfaces](../v2/adapter-surfaces.md#derived-work-encoding-and-test-seams).

```sh
cargo bench --locked -p vosu --bench state_bus_bench -- '^end_to_end/'
cargo bench --locked -p vosu --bench hot_paths -- 'publication|serializers'
cargo bench --locked -p vosu --bench state_bus_bench -- '^adapter/'
```

Build with `taskset -c 0-7,16-23 nice -n 10`, `RUSTC_WRAPPER=sccache` and
independent `CARGO_TARGET_DIR`/Criterion output directories per checkout.
Use the interleaved comparison tool above, one approved pinned physical core,
five paired rounds and an output artifact. Non-exclusive shared-machine runs
are exploratory, never official D17 evidence. Record commit/binary/harness,
profile/features, CPU/exclusivity, matched names, equivalent output, medians,
95% intervals, per-milestone and cumulative pre-M0 deltas; preserve inherited
M1/M2/M3 findings. The bar remains no >10% median regression judged with
non-overlapping 95% intervals. Record allocations, clocks, wakeups, CPU/tick
and source-read-to-real-frame latency for idle/subscribed profiles, separately
on native Windows stable/lazer and Linux lazer. Existing live reader `--bus`
mode is not adapter-frame evidence; X1 must add/document the actual adapter
output mode before measuring. No native/live/capture run is authorized by A4.

## M4a D17 status at the exit

**Status at the M4a exit (2026-11-24): D17 for M4a is pending. It is not a
pass.** Two measurements decide it, and neither has run:

1. Main's official pinned Criterion comparison of the frozen cases above, on
   one approved physical core with five paired rounds, independent targets
   and Criterion homes. It compares `end_to_end/adapter` and
   `end_to_end/adapter_precise` with the equivalent `end_to_end/legacy`
   workload at M3 `e1df38f` and pre-M0 `3a91ff9`, once that legacy harness
   and the `bench-compare.py` case alias below exist.
2. Native `live_reader_bench --adapter` runs (CPU/tick per stage and
   read-to-frame latency, idle and subscribed) on native Windows stable and
   lazer, and on Linux lazer. On Windows the stage CPU comes from
   `GetThreadTimes`, which charges whole ~15.6 ms scheduler ticks, so a
   per-tick median of a sub-millisecond stage reads 0 there. Use a mean
   over many ticks, or a finer thread clock, before comparing stages.

The [official Windows live adapter rows](#m4a-windows-live-adapter-results)
and [official Linux live adapter rows](#m4a-linux-live-adapter-results)
were measured at the M5 head. The paired legacy M3/pre-M0 adapter comparison
remains open.

The exploratory numbers in the next section decide nothing. They also predate
the exit merge with `v2/main`, which brought M1 L3's reader and read-plan
changes, so every case runs again on the final head.

**Inherited M1 rows, carried forward unchanged.** The owner's 2026-11-24
exception permits the M1 landing on `v2/main` only. Every row below is still
open and must close at Main's official M4a re-measurement or the M5 re-check,
or receive a fresh explicit exception
([source](#m1-planmemory-paired-d17)):

| M1 row (native Windows, source `1162fe6`) | Mean paired overhead | Status |
|---|---:|---|
| stable regular fixture, flat | +19.51% | Miss, open |
| stable precise fixture, flat | +30.81% | Miss, open |
| lazer regular fixture | — | Not established: no populated fixture |
| lazer precise fixture, flat | +10.26% | Miss, open |
| lazer precise fixture, routed | +13.61% | Miss, open |
| stable regular live | +22.07% | Miss, open |
| stable precise live | +7.36% | Point pass only; interval inconclusive |
| lazer regular live | +17.11% | Miss, open |
| lazer precise live | +31.11% | Miss, open |

The M2 and M3 D17 decisions are also still pending their quiet pinned runs
([M2](#m2-comparison-status), [M3](#m3-d17-contract)).

## M4a D17 implementation (exploratory, non-exclusive)

**No D17 pass is claimed.** These are short exploratory runs of X1 commit
`89824bc` on the shared Linux workstation (Ryzen 9 9950X3D, Rust 1.96.1,
release bench profile, default features), timed with `taskset -c 0 nice -n 10`
while other work used the host, CPU 0's SMT sibling not reserved, with
`--warm-up-time 0.5 --measurement-time 2 --sample-size 20`. Criterion home
`~/.cache/vosu-criterion/m4a-x1-exploratory`, target
`~/.cache/vosu-targets/m4a-x1`. No baseline binary was run, so no milestone or
cumulative delta is reported.

All cases share `benches/support/adapter/` (also used by the invariants): the
real osu! `BusPublisher` projection and ABI batch, host bus commit, the bundled
adapters' own surface requesters, the production `BusStateProvider` and
`WebSocketHub`. Inputs are `adapter/input.rs`: stable gameplay with scores
12345/23456 (regular) or `k1` counts 41/42 (precise-only; regular v2 emits no
keys). Each iteration publishes a different value and asserts a new encoding
or frame; exact view/encode/parse counts are asserted after timing through
instance-owned counters around the real adapters.

| Exact case | Timed work | Median [95% CI] |
|---|---|---|
| `end_to_end/adapter` | regular change → `/json/v2` HTTP bytes + encoded, delivered and acknowledged API v1 frame | 96.8 [90.6, 102.3] µs |
| `end_to_end/adapter_precise` | precise-only change → `/json/v2/precise` hub frame; regular bytes retained (same allocation, zero regular decodes) | 33.8 [32.3, 35.9] µs |
| `adapter/idle_no_clients` | one commit with four providers, four hubs and an API v1 hub but no client; zero adapter work and demand | 8.41 [8.33, 8.51] µs |
| `adapter/http_then_ws_cache` | change → HTTP bytes → WS frame sharing that allocation | 204.5 [201.0, 209.1] µs |
| `adapter/slow_consumer` | 16 commits while one frame is in flight, then ack → latest frame; one view/encoding | 1.147 [1.136, 1.155] ms |
| `adapter/focus_switch` | foreground switch between two stable sessions → HTTP + WS | 488 [461, 516] µs |
| `adapter/sc_unchanged_chart` | score change with the chart unchanged; zero chart opens/reads/parses | 199 [189, 210] µs |
| `adapter/plain_100_clients` | change fanned out to 100 acknowledging plain clients; one encoding | 8.34 [7.99, 8.70] ms |
| `adapter/filtered_clients` | 16 clients, 8 plans in pairs; 10.96 filtered encodings per change observed | 1.63 [1.48, 1.75] ms |

Cold chart work, reported separately from hot cases:
`adapter/sc_changed_chart` 654.7 [616.3, 710.8] µs per change: the osu!
cold-resource worker declares the songs root and publishes the new chart
reference, the SC worker opens, reads and parses it (4383 changes, exactly
4383 opens/reads/parses), then HTTP and WS share the derived bytes. Tiny
two-file fixture; OS page cache not flushed.

Unchanged comparison cases from the same run: `end_to_end/bus` 23.2, `bus_precise`
17.7, `api_frame` 18.1 µs; `serializers_cold/{v1,v2,v2_precise,stream_companion}`
3.08, 7.89, 0.234, 4.35 µs; `publication/unfiltered_clients/{1,100,1000}` 2.13,
2.08, 2.11 ms (synthetic hub only; dominated by its 1 ms cadence, not adapter
evidence).

**Exploratory findings for the official run.** `plain_100_clients` costs about
83 µs per client although one encoding is shared: every transport
acknowledgement runs `wake_writable`, which captures the surface selection and
compares it with the cached image. `http_then_ws_cache` (204 µs) is likewise
about twice `end_to_end/adapter`. Filtered pairs share bytes only when both
acknowledgements reach one delivery pass. These are measurements, not yet
attributed regressions.

The 1.x `end_to_end/legacy` workload over these same inputs is **not built
yet**: the benchmark-only legacy harness commits for pre-M0 `3a91ff9` and M3
`e1df38f`, and a `scripts/bench-compare.py` case-alias option to pair
`end_to_end/adapter` with `end_to_end/legacy`, remain to be added. Compare only
case names present in every binary until then.

### Live adapter-output mode

`examples/live_reader_bench.rs --adapter` loads the production osu! grants and
the bundled upstream adapter in a private host attached to the live client.
Per lane (regular → `/json/v2`, precise → `/json/v2/precise`):

- `adapter_stages`: median tick-thread CPU per tick by stage: `read` (capture
  plan execution), `reduce`, `projection` (projection, diff, batch encoding),
  `commit` (host commit inside the synchronous publish call), `surface_view`
  (coherent capture plus typed decode) and `encode`. A tick whose captured
  selection equals the previous one is a cache hit, as in the provider.
- `adapter_frame`: wall time from read start to the real hub frame, delivered by
  acknowledgement/wake; it excludes the 1.x cadence wait (up to one poll
  interval) an idle, already-acknowledged client sees.

These stages attribute the residual left open in the
[M1 PlanMemory](#m1-planmemory-paired-d17) exception. `--bus` remains bus-only.

```powershell
# Native Windows, release examples, client in gameplay; repeat idle in menu.
cargo build --release --locked --no-default-features --example live_reader_bench
target\release\examples\live_reader_bench.exe --pid <stable-pid> --client stable --adapter
target\release\examples\live_reader_bench.exe --pid <stable-pid> --client stable --adapter --memory plan
target\release\examples\live_reader_bench.exe --pid <lazer-pid> --client lazer --adapter
target\release\examples\live_reader_bench.exe --pid <lazer-pid> --client lazer --adapter --memory plan
```

Linux lazer: `cargo build --release --locked --example live_reader_bench`, then
`target/release/examples/live_reader_bench --pid <pid> --client lazer --adapter`
(add `--memory plan`). Each JSON result holds both lanes and both modes.

### M4a Windows live adapter results

Same native Windows window, exact `ad724666`, stable `b20261118.1` and
lazer `2026.1117.0`. `--adapter` read native and PlanMemory paths in
the main menu (state 0) and in Autoplay (state 2) on the synthetic
`vosu D17 Replay` chart. There were no reported capture errors in the
accepted rows. These are current-head live measurements, not a matched
legacy or M4a-exit-revision comparison.

All times are µs. CPU data are medians of nine 1,000-tick batch averages
from `GetThreadTimes`. Stage order is read / reduce / projection / commit /
surface view / encode. Stage medians need not sum to the total median.
Frame cells report the median read-to-actual-hub-frame latency and
the number of changed frames measured; `—` indicates no new frame.
The harness reports min and max, not confidence intervals, and does
not expose allocation, clock-read or wakeup counters in this mode.

| Client | State / memory | Regular CPU | Regular stages | Regular frame | Precise CPU | Precise stages | Precise frame |
|---|---|---:|---|---:|---:|---|---:|
| Stable | Menu / native | 421.9 | 46.9/328.1/0/0/31.2/0 | 867.6 (5,417) | 15.6 | 0/0/0/0/0/0 | — (0) |
| Stable | Menu / plan | 406.2 | 46.9/312.5/0/0/31.2/0 | 1,078.3 (6,220) | 15.6 | 0/0/0/0/15.6/0 | — (0) |
| Stable | Play / native | 375.0 | 46.9/265.6/0/0/62.5/15.6 | 841.4 (6,418) | 31.2 | 31.2/0/0/0/0/0 | 154.6 (4) |
| Stable | Play / plan | 390.6 | 62.5/250.0/0/0/62.5/15.6 | 882.2 (7,209) | 31.2 | 31.2/0/0/0/0/0 | 219.2 (4) |
| Lazer | Menu / native | 390.6 | 46.9/296.9/0/0/31.2/0 | 887.3 (5,854) | 15.6 | 15.6/0/0/0/0/0 | — (0) |
| Lazer | Menu / plan | 421.9 | 62.5/312.5/0/0/15.6/15.6 | 935.6 (6,803) | 15.6 | 15.6/0/0/0/0/0 | — (0) |
| Lazer | Play / native | 390.6 | 31.2/328.1/0/0/15.6/0 | 912.5 (3,438) | 31.2 | 15.6/0/0/0/15.6/0 | 278.5 (6) |
| Lazer | Play / plan | 406.2 | 62.5/328.1/0/0/31.2/0 | 959.4 (4,055) | 31.2 | 15.6/0/0/0/0/0 | 197.5 (6) |

Zero stage charges and the four/six precise gameplay frames per row are
not enough to assign a precise regression verdict. These measurements
do not close the inherited M1 or M4a comparison gates.

### M4a Linux live adapter results

Same pinned Linux VM window (`vosu-gaming.service`, exact `ad724666`, native
offline lazer `2026.1117.0`, reader pinned to CPU `6` with explicit
`CAP_SYS_PTRACE`), main menu (state 0) and Autoplay (state 2) on `vosu D17
Replay`, 0 capture errors. All times are µs; stage order is read / reduce /
projection / commit / surface view / encode (medians of nine 1,000-tick
batch averages). Frame cells report median read-to-hub-frame latency and
changed-frame count.

| Client | State / memory | Regular CPU | Regular stages | Regular frame | Precise CPU | Precise stages | Precise frame |
|---|---|---:|---|---:|---:|---|---:|
| Lazer | Menu / native | 195.1 | 47.1/20.2/3.6/2.5/95.4/25.1 | 595.0 (5,166) | 15.0 | 7.8/0.4/1.3/0.0/5.5/0.0 | — (0) |
| Lazer | Menu / plan | 221.2 | 68.5/22.1/6.6/2.9/97.4/25.5 | 617.1 (5,062) | 18.1 | 10.8/0.5/1.2/0.0/5.4/0.0 | — (0) |
| Lazer | Play / native | 165.1 | 50.1/19.9/1.4/1.3/84.1/7.1 | 535.8 (1,702) | 18.6 | 10.8/0.6/1.2/0.0/5.7/0.0 | 59.9 (3) |
| Lazer | Play / plan | 176.3 | 64.9/19.5/1.3/1.2/82.1/7.2 | 602.2 (1,822) | 22.1 | 14.7/0.5/1.4/0.1/5.7/0.0 | — (0) |

These are current-head live observations, not a matched M3/pre-M0 legacy
comparison; the inherited M1 and M4a comparison gates stay open.

### streamCompanion HTTP chart wait bound (X0)

HTTP-only `/json/sc` readers keep chart facts: releasing the last consumer no
longer cancels the selected key's chart job or drops its completed result,
and an HTTP read whose exact-input job is in flight waits at most
`DERIVED_HTTP_WAIT` = **80 ms** (`crates/vosu-core/src/server/bus_provider.rs`) before
falling back to the pinned chart-less bytes. The bound is 2 × the largest
measured p99 and stays under the 500 ms cap. Measured 2026-11-24 with a
throwaway release-build test (not a committed benchmark), Ryzen 9 9950X3D
pinned to CPUs 0-7, **non-exclusive**, 60 cold `BeatmapCache::load` +
projection runs per map; resource open/read through the retained root is
excluded:

| Map | Size | Median | p99 | Max |
| --- | --- | --- | --- | --- |
| source-oracle fixture chart (1 object) | 226 B | 0.11 ms | 0.23 ms | 0.27 ms |
| `crates/vosu-pp/rosu-pp-api/resources/2785319.osu` | 33,573 B | 3.52 ms | 4.62 ms | 4.94 ms |
| `Within Temptation - The Unforgiving (Armin) [Marathon].osu` | 243,197 B | 36.68 ms | 40.08 ms | 40.17 ms |

Slower hosts that exceed the bound on a very large map serve chart-less bytes
for that first read only; the job still completes and later reads reuse its
result. X1's `adapter/sc_unchanged_chart` and `adapter/sc_changed_chart`
cases measure this path and must be re-baselined after this change.

## M3 D17 contract

**Status at the M3 exit (2026-11-24): D17 for M3 is pending.** The official
pinned comparison has not run yet. The machine was not quiet during the exit,
so the run was deferred to a scheduled window, using the retained
`m3-x1-m2-exact` and `m3-x1-prem0` checkouts. Until it runs, the
provisional results below decide nothing.

**No D17 pass is claimed.** X1 implementation `4f0c6654cb` (base
`fde820d`) was measured on the shared Linux workstation, pinned to logical CPU
0, **non-exclusive**: SMT sibling 16 remained available to other work. Builds
used `taskset -c 0-7,16-23 nice -n 10`; measurements used CPU 0 and `nice -n 10`.
The Windows VM's CPUs were not used. Native Windows and live idle/subscribed
CPU observations are not established by these synthetic Linux results.

The configured Rust 1.96.1 release/bench profile uses thin LTO and one codegen
unit. Root defaults were used, without the release-forbidden `test-harness`
feature. Dev-only `bench-harness` exposes deterministic foreground injection
and the four plugin fixture modules. The command
`cargo tree -e features -p vosu --no-default-features --features linux-process-events --edges normal`
reported no `bench-harness` or `vosu-plugin-testkit` in the normal daemon graph
(full output: `/tmp/m3-x1-normal-features.txt`).

### New API work: hot publication, reads and delivery

The following are Criterion medians [95% confidence intervals], in **µs**.
These short provisional runs used 50 ms warmup, 100 ms measurement and ten
samples. All twelve named cases ran successfully through the acceptance filter.

| Exact case | Median [95% CI], µs |
|---|---|
| `api_v1/idle_no_clients` | 1.270 [1.268, 1.286] |
| `api_v1/http_current_read` | 0.960 [0.958, 0.960] |
| `api_v1/subscribe_regular` | 7.857 [6.017, 7.995] |
| `api_v1/subscribe_precise` | 8.436 [8.420, 8.459] |
| `api_v1/subscribe_slow_consumer` | 64.023 [63.297, 80.502] |
| `api_v1/active_rebind` | 25.940 [25.724, 26.303] |
| `api_v1/reference_publish_unchanged` | 2.526 [2.509, 2.574] |
| `api_v1/reference_publish_changed` | 2.540 [2.532, 2.546] |
| `api_v1/resource_cached_range` | 20.926 [20.859, 27.723] |
| `api_v1/stream_range` | 23.458 [23.381, 23.779] |
| `api_v1/job_progress_poll` | 0.630 [0.626, 0.631] |
| `api_v1/end_to_end` | 13.730 [13.701, 13.773] |

Fixtures use distinct live session/plugin identities and nonzero scores. The
regular and precise cases use the real core source and API hub, consuming
actual encoded frames; precise selects `/vosu.osu/beatmap/time/live`. A slow
iteration holds its previous frame in flight across sixteen commits before
acknowledging and receiving again. Active rebinding includes the fresh initial
frame. Reference cases call core's `commit_from_instance` with pre-encoded
references and alternating content IDs, without any roots, files or resolver;
they do not depend on the osu! production publisher's O2 wiring. Cached file
and stream cases consume a six-byte range through the resource router and body.
Job progress reserves a real waiting poll, publishes progress, awaits its
notification and serializes the resulting snapshot. The end-to-end case uses
`BusPublisher` projection, host ABI commit and actual API frame delivery.
`state_bus_bench` also exposes that path as `end_to_end/api_frame`.

### Cold resource identity IO (not a hot commit average)

`resource_identity_cold_io/stable_chart`: **14.105 [14.056, 14.764] µs**.
Each operation submits a fresh selection to the production `ResourceWorker`,
opens the retained root/child and recomputes the mutable-file metadata identity.
The fixture is the 36-byte `tests/fixtures/api-v1-plugin/fixture.osu`. This is
cold *worker/open/identity* work; OS page caches were not flushed, and this tiny
fixture does not characterize bulk-byte hashing or large immutable snapshots.
No such IO is hidden in the reference commit numbers above.

### Like-for-like milestone and cumulative comparisons

All pairs used `scripts/bench-compare.py --no-reserve --cpu 0 --rounds 2
--warmup 0.05 --measurement 0.1 --samples 10`. Every label/round has an
independent Criterion home. Independent targets were `m3-x1`,
`m3-x1-m2-exact` and `m3-x1-prem0` beneath
`/home/user/.cache/vosu-targets/`; no cross-revision Cargo target was shared.

Historical production sources are M2 `80a1f82` and pre-M0 `3a91ff9`.
`/home/user/checkouts/m2-l1` had advanced to `a3732be`, so the reported M2
comparisons instead use the isolated `m3-x1-m2-exact` checkout at exactly the
requested revision. The discarded advanced-source trial is not in these tables.
Benchmark-only adapters are retained as local commits `1453e77ac6` (M2) and
`dd5d524d47` (pre-M0); neither changes production sources.

The first table retains the existing identical hot-path fixtures. Entries are
medians of round medians [bootstrap 95% intervals], **µs**. Two short busy-host
rounds produce weak intervals; apparent improvements are not a performance claim.

| Common case | Pre-M0 | M2 | M3 | M3 vs M2 / pre-M0 |
|---|---|---|---|---|
| `publication/unfiltered_clients/1` | 2059.958 [2057.320, 2062.597] | 2058.634 [2055.279, 2061.988] | 2058.268 [2054.745, 2061.792] | -0.02% / -0.08% |
| `publication/unfiltered_clients/100` | 2076.799 [2065.105, 2088.493] | 2069.025 [2064.789, 2073.262] | 2068.291 [2064.411, 2072.171] | -0.04% / -0.41% |
| `publication/unfiltered_clients/1000` | 2164.887 [2142.909, 2186.866] | 2110.244 [2060.039, 2160.450] | 2117.121 [2081.281, 2152.962] | +0.33% / -2.21% |
| `serializers_cold/stream_companion` | 1.614 [1.501, 1.728] | 1.341 [1.216, 1.466] | 1.259 [1.206, 1.312] | -6.13% / -22.02% |
| `serializers_cold/v1` | 1.009 [0.905, 1.112] | 1.352 [1.194, 1.510] | 0.919 [0.877, 0.962] | -32.01% / -8.83% |
| `serializers_cold/v2` | 3.169 [2.840, 3.499] | 2.601 [2.275, 2.927] | 2.383 [2.260, 2.507] | -8.36% / -24.79% |
| `serializers_cold/v2_precise` | 1.061 [0.989, 1.133] | 0.887 [0.799, 0.975] | 0.828 [0.797, 0.859] | -6.62% / -21.95% |

Only common `state_bus_bench` cases are paired; no pre-M0 bus or historical
API-v1 case was invented.

| Common case | M2, µs [95% CI] | M3, µs [95% CI] | M3 vs M2 |
|---|---|---|---|
| `end_to_end/bus` | 25.152 [20.254, 30.051] | 30.621 [30.480, 30.762] | +21.74% |
| `end_to_end/bus_precise` | 19.012 [15.004, 23.020] | 17.669 [14.310, 21.027] | -7.07% |
| `end_to_end/legacy` | 1.947 [1.640, 2.254] | 1.382 [1.354, 1.410] | -29.02% |

**Unresolved provisional finding:** `end_to_end/bus` is +21.74%, with
non-overlapping recorded intervals. Shared-host noise is not grounds to erase
this result or declare a pass; the official quiet run must resolve it.
The existing M1 PlanMemory daemon-CPU finding and M2 performance status below
remain open and are not superseded by these measurements.

For new API work, a common semantic workload selects the same score 12345 from
an active stable snapshot (regular and precise status 2), then encodes exactly
`{"value":12345}`; both harnesses assert those bytes. Historical builds use
`ProductionSnapshotEncoder::subscription_document` with `/play/score`; M3 uses
the immutable API source with `/vosu.play/score`. This measures selected-read
and serialization work, **not** HTTP transport or publication. The adapter's
only source differences are the imports needed by each historical layout.

| Equivalent selected-read case | Pre-M0, µs [95% CI] | M2 | M3 | M3 vs M2 / pre-M0 |
|---|---|---|---|---|
| `selection_equivalent/score_read` | 3.589 [3.367, 3.810] | 3.162 [2.600, 3.724] | 0.213 [0.204, 0.221] | -93.28% / -94.07% |

### Cost invariants and reproduction

The real host publication path allocates **672 times per 32 commits** both
without and with an idle API hub: **zero incremental hub allocations** (not
zero total commit allocations). Tests also assert zero new idle API tasks,
zero unobserved API notification revisions and no subscription demand. A real
connection is the positive control for the task counter. With no API producer
task/timer, there are no API timer-driven clock reads; this is not a global
daemon clock-call count. A separate test holds a real frame in flight across
1024 commits, rejects a queued second frame and receives the distinct latest
score after acknowledgment. Reference tests distinguish stable and changed IDs
with no filesystem/resolver available. Every new test wait is bounded.

```sh
cargo test --locked -p vosu --features test-harness \
  --test performance_invariants --test runtime_wakeups api_v1 -- --nocapture
cargo bench --locked -p vosu --bench api_v1_bench -- '^api_v1/'
cargo bench --locked -p vosu --bench state_bus_bench -- 'end_to_end'
cargo bench --locked -p vosu --bench hot_paths -- 'publication|serializers'
```

Provisional raw Criterion homes are
`/home/user/.cache/vosu-criterion/m3-x1-api-accepted` (twelve API cases) and
`m3-x1-api-evidence` (cold IO) under that same parent. Paired `summary.json`
files and per-round logs are in `m3-x1-hot-exact`, `m3-x1-bus-exact` and
`m3-x1-selection-exact` there. Each JSON records exact binaries and
`"exclusive": false`. The three acceptance commands passed with the short
Criterion timing options above. Linux/Windows cross-Clippy, formatting, 1252
nextest tests (3 skipped), and applicable source/boundary checks passed.

### Official quiet-window commands (not run here)

First rebuild each retained checkout with its independent target named above,
using `taskset -c 0-7,16-23 nice -n 10 cargo bench --locked -p vosu --bench ... --no-run`.
The exact retained binaries are set below. Run the comparisons only in the
approved quiet window: **omit `--no-reserve`**, so the established runner reserves
the physical core and records `exclusive: true`. The repeated M3-only label is
a reproducibility control for new cases, not a fabricated historical baseline.
Use a new output directory on every invocation.

```sh
T=/home/user/.cache/vosu-targets
P0="$T/m3-x1-prem0/release/deps"
M2="$T/m3-x1-m2-exact/release/deps"
M3="$T/m3-x1/release/deps"
BENCH_CPU=0
OUT=/home/user/.cache/vosu-criterion/m3-official-$(date +%Y%m%d-%H%M%S)
PRE_HOT="$P0/hot_paths-e79c7b2ab73c306c"
M2_HOT="$M2/hot_paths-34cc5803d3cae8b6"
M3_HOT="$M3/hot_paths-400d5cc2ce187904"
M2_BUS="$M2/state_bus_bench-83994cd16dfb1cbc"
M3_BUS="$M3/state_bus_bench-8b20e9d1f303d96f"
PRE_SELECT="$P0/api_selection_bench-3e064ffb3ceac248"
M2_SELECT="$M2/api_selection_bench-6cbc05ee45cef411"
M3_API="$M3/api_v1_bench-268f9f7916968fb5"
sudo python scripts/bench-compare.py --cpu "$BENCH_CPU" --rounds 5 \
  --binary "prem0=$PRE_HOT" --binary "m2=$M2_HOT" --binary "m3=$M3_HOT" \
  --filter 'publication|serializers' --output "$OUT/hot"
sudo python scripts/bench-compare.py --cpu "$BENCH_CPU" --rounds 5 \
  --binary "m2=$M2_BUS" --binary "m3=$M3_BUS" \
  --filter '^end_to_end/(bus|bus_precise|legacy)$' --output "$OUT/bus"
sudo python scripts/bench-compare.py --cpu "$BENCH_CPU" --rounds 5 \
  --binary "prem0=$PRE_SELECT" --binary "m2=$M2_SELECT" --binary "m3=$M3_API" \
  --filter '^selection_equivalent/score_read$' --output "$OUT/selection"
sudo python scripts/bench-compare.py --cpu "$BENCH_CPU" --rounds 5 \
  --binary "m3=$M3_API" --binary "m3-repeat=$M3_API" \
  --filter '^api_v1/' --output "$OUT/api"
sudo python scripts/bench-compare.py --cpu "$BENCH_CPU" --rounds 5 \
  --binary "m3=$M3_API" --binary "m3-repeat=$M3_API" \
  --filter '^resource_identity_cold_io/' --output "$OUT/cold-io"
```

The runner defaults to 3 s warmup, 10 s measurement and 100 samples. Pair the
live idle/subscribed daemon observations separately; these synthetic cases do
not replace them. D17 still requires resolving >10% non-overlapping regressions
per milestone and cumulatively, plus the outstanding live/native evidence.

## Performance changes for M2

`state_bus_bench` measures the production `BusPublisher` projection/diff/flat
encoding path, not a second implementation of the osu! projector:

| Group | Timed work |
|---|---|
| `bus_commit/regular_batch`, `precise_batch` | Borrowed decode and `StateStore::commit` of a pre-encoded incremental batch |
| `osu_projection/cold` | Fresh publisher projection, diff, encoding and capture sink |
| `osu_projection/incremental_regular`, `incremental_precise` | Alternating one changed source lane, warmed publisher and reusable encoding buffers |
| `osu_projection/unchanged` | Same regular/precise Arcs; no publication |
| `end_to_end/bus`, `bus_precise` | Both-lane or precise-only snapshots → projector → flat batch → commit → `api_json` → changed selection and one `STATE_UPDATE` encoding for a ten-selector subscriber |
| `end_to_end/legacy` | The same gameplay snapshot → uncached v2 and precise serializers |

The M2 measurements below are historical pre-M4a evidence. Their legacy path
used a fresh root snapshot per iteration. That production architecture has
been removed; current benchmark fixtures use actual producer → ABI bus →
authorized surface → adapter encoding. Changed workloads need new matched-case
measurements before comparing against these historical values.

```sh
cargo bench --locked --package vosu --bench state_bus_bench
cargo bench --locked --package vosu --bench hot_paths -- 'publication|serializers'
cargo nextest run --locked --features test-harness --test performance_invariants --test state_bus_invariants
```

Criterion accepts one regex filter; the plan's literal two positional filters
`publication serializers` are rejected, so the command above selects their union.

The invariants use the existing thread-local counting allocator: an active
publisher must allocate and publish nothing for repeated unchanged source Arcs,
allocate nothing for warmed alternating precise ticks, and keep repeated scalar
commits under a fixed allocation ceiling while actually replacing values.

### Projection skipping

The projector retains only its existing previous snapshot. Equal lane Arcs skip
their input groups; changed regular Arcs compare their owned source groups
without cloning them. An exhaustive dependency table preserves cross-lane reads:
precise status gates regular gameplay/mods/tourney, and progress also reads the
regular beatmap. Skipped pointers retain their diff values; generation, reset,
refusal and unwind invalidation still visit every pointer on the next attempt.

The opt-in diagnostic times source equality against batched production
encoder/diff callbacks, without timing assertions:

```sh
cargo test --release --locked -p vosu-osu --lib projection_group_costs -- --ignored --nocapture
```

Shared-host release results on default snapshots (ns). Encoder work forced by
the always-dirty result group is excluded from other groups' savings. Remaining
dependent pointer sets overlap, exclude dispatch/input lookup, and must not be summed:

| Source | Compare | Skippable encode + diff |
|---|---:|---:|
| global | 654.39 | 980.91 |
| settings | 0.44 | 2496.44 |
| beatmap | 259.00 | 2254.75 |
| menu | 204.15 | 1378.64 |
| bass density | 0.67 | 96.33 |
| gameplay / leaderboard | 785.62 | 1129.98 |
| result screen | 647.50 | 640.01 |
| user | 0.55 | 57.10 |
| lazer spectating / tourney | 0.55 | 22.31 |
| ranked play | 0.75 | 10.52 |
| room | 0.50 | 10.34 |

Result-screen equality was too close to encoding cost, so that group is gated
only by regular-lane identity, not by group equality. These empty/default inputs
are not a large-history or populated-lobby performance guarantee.

### Live B1 and bus ticks

```sh
cargo build --release --locked --example live_reader_bench
taskset -c 7 target/release/examples/live_reader_bench \
  --pid CLIENT_PID --client lazer --memory native --bus --warmup 100 --cycles 1000
```

`--bus` attaches a real in-process plugin host and a real `vosu-surface`
subscription. For both regular and precise lanes it compares B1-only (the M1
publication shape) with B1 + bus. It measures the current tick thread's CPU
clock around capture, reducer and synchronous publication, not elapsed wall
time relabelled as CPU. Both modes use the production `B1Publisher` and the
same latest-only sink. **B1 manager background work, other daemon threads,
poll scheduling, and network delivery are excluded**; this is not whole-process
daemon CPU usage. Run the historical M1 binary separately for a revision-level
live comparison; the in-binary B1-only mode isolates the added bus cost.
As in the capture-only example, PP calculation is disabled in both modes.

Read-to-commit latency starts immediately before capture and ends when the
surface subscriber observes the committed revision. It is wall time and includes
the synchronous publisher return and subscriber-observation overhead. Unchanged
ticks have no update and are counted separately, never reported as zero latency.
Each observation wait is bounded. Reject errors, changed client states, or a run
without changed-tick latency observations.

`--memory plan` uses the host read-plan adapter; `native` uses the OS reader even
when the bus host is attached. On Windows the example uses the daemon's
`<release directory>/.cache/offsets`, not `examples/.cache/offsets`; Linux retains
the platform cache root. No VM is required to build or run these instruments.

### M2 comparison status

Shared-host comparisons must use `scripts/bench-compare.py --no-reserve`, separate
target directories for HEAD, M1 `ec2d086`, and pre-M0 `3a91ff9`, and fresh Criterion
homes per invocation. The bus does not exist at either historical revision:
their executable comparison is the identical legacy serializer fixture, not a
backported M2 bus mislabeled as historical production. Preserve `exclusive:
false` and each comparison's `inconclusive` flag in the evidence.

**D17 is not established by shared-host runs.** A quiet pinned-core run with
non-overlapping 95% intervals is still required before claiming the 10% milestone
and cumulative bar.

The X2b projection-skipping comparison uses unmodified M2 `fdf045c` versus the
skip implementation, with the identical additive `bus_precise` instrument in
both binaries. Separate targets: `m2-x2b-baseline` and `m2-x2b`. CPU 7, sibling
23 unreserved, three interleaved rounds, 1 s warm-up, 2 s measurement, 50 samples,
`scripts/bench-compare.py --no-reserve --filter 'bus_commit|osu_projection|end_to_end'`.
All rows have `exclusive: false`; intervals below bootstrap the round medians,
not Criterion samples. Raw evidence: `m2-x2b/comparison/summary.json` under the
local Cargo-target cache. These are **not a D17 milestone/cumulative pass**.

| M2 case | Before [95% interval], µs | After [95% interval], µs | Delta | Inconclusive |
|---|---:|---:|---:|---|
| `bus_commit/regular_batch` | 0.778 [0.764, 0.806] | 0.790 [0.771, 0.852] | +1.50% | yes |
| `bus_commit/precise_batch` | 0.889 [0.886, 0.911] | 0.882 [0.864, 0.906] | −0.87% | yes |
| `osu_projection/cold` | 7.877 [7.866, 8.005] | 8.118 [7.736, 14.115] | +3.06% | yes |
| `osu_projection/incremental_regular` | 9.417 [9.336, 10.449] | 5.283 [5.211, 5.439] | −43.90% | no |
| `osu_projection/incremental_precise` | 9.362 [9.312, 9.372] | 0.697 [0.686, 0.701] | −92.56% | no |
| `osu_projection/unchanged` | 0.049 [0.049, 0.055] | 0.045 [0.043, 0.054] | −8.04% | yes |
| `end_to_end/bus` | 25.974 [24.913, 33.163] | 21.155 [21.141, 21.163] | −18.55% | yes |
| `end_to_end/bus_precise` | 24.358 [24.231, 25.339] | 15.571 [15.463, 15.627] | −36.07% | no |
| `end_to_end/legacy` | 1.641 [1.633, 1.681] | 1.673 [1.671, 1.685] | +1.94% | yes |

The incremental commit fixture clears the captured cold batch before projecting
the changed snapshot and asserts that a new batch was sent. Both capture lanes
carry gameplay status, so a suppressed score change cannot accidentally reuse
the cold batch. Earlier estimates made before that guard are discarded.

The paired run used CPU 7 (SMT sibling 23 **not** reserved), five interleaved
rounds, 0.5 s warm-up, 1 s measurement and 30 samples, via
`scripts/bench-compare.py --no-reserve`. Independent targets were `m2-x2-prem0`,
`m2-x2-m1`, and `m2-x2`; M2's measured fixture is `aa14d2d1b7`. Both historical
checkouts had only the legacy benchmark/manifest added; their production code
remained at the named commits. All rows have `exclusive: false`.

| Case | `3a91ff9`, µs [95% CI] | M1 `ec2d086`, µs [95% CI] | M2, µs [95% CI] | M2 vs M1 / inconclusive | M2 vs pre-M0 / inconclusive |
|---|---|---|---|---|---|
| `end_to_end/legacy` | 1.5890 [1.5826, 1.7435] | 1.4936 [1.4874, 1.6451] | 1.6472 [1.6369, 1.8007] | +10.28% / true | +3.67% / true |
| `publication/unfiltered_clients/1` | 2069.9824 [2057.6718, 2083.3083] | 2082.4614 [2063.4970, 2083.3489] | 2082.9160 [2071.6655, 2083.2494] | +0.02% / true | +0.62% / true |
| `publication/unfiltered_clients/100` | 2083.2132 [2082.0651, 2083.3373] | 2083.1093 [2081.3086, 2083.2977] | 2082.6621 [2081.5812, 2083.3391] | -0.02% / true | -0.03% / true |
| `publication/unfiltered_clients/1000` | 2084.1429 [2051.0231, 2094.7761] | 2083.3933 [2079.4761, 2110.0448] | 2088.5412 [2080.5258, 2104.7634] | +0.25% / true | +0.21% / true |
| `serializers_cold/stream_companion` | 1.3728 [1.3611, 1.3825] | 1.3573 [1.3424, 1.3920] | 1.3718 [1.3525, 1.5485] | +1.07% / true | -0.07% / true |
| `serializers_cold/v1` | 0.9789 [0.9658, 1.0123] | 0.9949 [0.9801, 1.0046] | 0.9840 [0.9774, 1.0002] | -1.09% / true | +0.52% / true |
| `serializers_cold/v2` | 2.4318 [2.4150, 2.7425] | 2.4748 [2.4436, 2.4834] | 2.4296 [2.4122, 2.4834] | -1.83% / true | -0.09% / true |
| `serializers_cold/v2_precise` | 0.8834 [0.8778, 0.9070] | 0.8940 [0.8906, 0.8993] | 0.8777 [0.8666, 0.9376] | -1.82% / true | -0.65% / true |

Numbers are medians of round medians and bootstrap 95% intervals. M2-vs-M1
flags apply the comparison tool's identical spread/overlap rule to the same
recorded rounds. Every comparison is **inconclusive**, including the +10.28%
legacy milestone delta; neither passing nor failing D17 is established.
The new bus path is materially more work in this fixture than the legacy
serializers, so its raw smoke estimate must not be presented as satisfying D17.

Local evidence (full round logs, raw estimates and summaries):
`/home/user/.cache/vosu-bench-results/m2-x2-legacy/summary.json` and
`/home/user/.cache/vosu-bench-results/m2-x2-hot-paths/summary.json`.
Actual live-client CPU/latency and Windows-native runs were not performed;
the unit's no-VM constraint was retained. The synthetic real-host surface test,
allocation invariants, Linux Clippy and Windows cross-Clippy passed.

### Live reader: native versus plan

The dev-only example attaches read-only to an existing client PID and measures
production regular and precise capture adapters, without network publication.
`--memory native` opens the OS backend directly (the default); `--memory plan`
attaches the same PID to an in-process bundled SDK plugin and reads through
`PlanProcessMemory`, the real host and SDK. Signature scans use `PlanScanBackend`.
Setup is excluded; nine sample means follow warm-up. Keep the client in the
same state for both modes, and reject runs with errors or a changed state.
Lazer needs cached or embedded offsets for its exact version; run the daemon
once to populate `plugins/vosu.osu/offsets` under its data directory if needed.
On Linux this is normally `~/.local/share/vosu/plugins/vosu.osu/offsets`;
on Windows it is beside the daemon, e.g. `target\release\plugins\vosu.osu\offsets`,
not inside `target\release\examples`. Read/byte counters remain null.

Windows VM (PowerShell, repository root; run once for each `$client`):

```powershell
$client = 'stable' # repeat with 'lazer'; close the previous client first
& V:\osu-setup\launch.ps1 -Client $client
$clientPid = (Get-CimInstance Win32_Process | Where-Object Name -eq 'osu!.exe' | Sort-Object CreationDate -Descending | Select-Object -First 1).ProcessId
if (-not $clientPid) { throw 'osu! client not running' }
cargo build --release --locked --no-default-features --example live_reader_bench --target-dir target
foreach ($memory in @('native', 'plan')) {
    cmd /c "start `"`" /b /wait /affinity 4 target\release\examples\live_reader_bench.exe --pid $clientPid --client $client --memory $memory --warmup 100 --cycles 1000 --samples 9"
    if ($LASTEXITCODE) { throw "reader bench failed: $memory" }
}
```

Linux VM (native lazer installed under `/opt/osu`; launch in the guest desktop,
or use its existing headless launcher `/opt/osu/osu-headless`):

```sh
/opt/osu/osu
# In another guest terminal, from the repository root:
pgrep -af '/opt/osu|osu!'
read -r -p 'Target lazer game PID (not wrapper): ' client_pid
cargo build --release --locked --example live_reader_bench --target-dir target
for memory in native plan; do
  taskset -c 2 target/release/examples/live_reader_bench \
    --pid "$client_pid" --client lazer --memory "$memory" \
    --warmup 100 --cycles 1000 --samples 9 || exit
done
```

Repeat in reverse mode order for a paired comparison. These capture timings
are not whole-daemon CPU/tick or read-to-network-frame latency measurements.
Regular captures reuse the beatmap head and track length from `global`; the
reader integration excludes the separate `ConfigPlan` experiment.

## Plugin runtime wakeups

Reproduce on Linux (debug/test harness, no game or VM required):

```sh
nice -n 10 cargo test --locked --all-features --test runtime_wakeups -- --include-ignored --nocapture --test-threads=1
```

The populated stable fixture runs the real readers, reducer and synchronous B1
publisher. Stable Tokio's unpark hook counts runtime wakes; a test-harness-only
future wrapper counts reducer polls without `tokio_unstable`. At 150 ms regular /
10 ms precise cadence in the menu, 50 publications took 0.475 s before and
0.473 s after: **105 → 53 runtime wakes**, **53 → 53 reducer polls**. The change
uses one scheduler worker instead of two and removes the reducer's unrelated
10 ms cancellation timer (shutdown already aborts it). Reader cadence is unchanged.
With 100 ms precise sampling and regular captures held outside the sample window,
the resulting single-worker runtime has exactly 50 wakes and 50 polls for 50
publications; this case guards against reintroducing timer-only wakes.

Eight tournament-like stable sessions, 150/10 ms cadence, 100 precise samples per
session after 20 warmups: two-worker read-completion-to-publication p50 spans
14.408–23.364 µs and p99 74.501–250.282 µs; one-worker p50 spans
15.329–34.425 µs and p99 98.837–269.178 µs. The largest paired per-session p99
increase is 190.900 µs, below one precise tick (10 ms). This synthetic case has
PP enabled but no successfully loaded beatmap; PP/progressive calculation runs
on the blocking pool, not the scheduler worker. These are not network or live
Windows CPU measurements. The real ConcurrentPlugin stress test also passes
with one worker (32 sessions, 16 persistent anchors, eight concurrent producers).

## Scope of the synthetic results

- Synthetic fixtures, not live-game or whole-daemon performance. The stable raw
  snapshot and PlanMemory cases share a populated x86 graph and assert successful,
  non-default global/full-menu/gameplay values before timing. Lazer's raw snapshot
  regular fixture remains zero-filled. `plan_memory/lazer_precise_*` shares the
  populated active lazer graph with the deterministic reader, including 18 keys.
  Windows fixture reads use native self-process RPM; Linux uses slice copies.
- Calculation uses a five-object map. Cold serializers exclude setup; cache
  cases clone encoded bytes. Publication includes timer scheduling; neither
  publication nor subscriptions measures network transport.
- Live-client and released-tosu measurements are in the
  [released tosu comparison](#released-tosu-comparison).

## M1 PlanMemory precise capture

Measured 2026-11-24 on the Ryzen 9 9950X3D with Rust 1.96.1, locked
dependencies and default Linux features. Baseline is `1b96950`; candidate is
the precise-plan follow-up on `m1/precise-plan`. Both executables contain the
same added precise benchmark. Baseline uses a separate ovnfs checkout and
`CARGO_TARGET_DIR`; no cross-revision Cargo artifacts are shared.
Builds used `taskset -c 0-7,16-23 nice -n 10`, `RUSTC_WRAPPER=sccache`, and
candidate target `/home/user/.cache/vosu-targets/m1-pp`.

The populated stable x86 graph is shared with `hot_paths`. Before timing,
both transports must return identical successful global/menu/gameplay values
and precise status **3**, time **0** (song selection deliberately suppresses
play time). The new precise cases exercise `global_precise`'s warmed pointer
and value batch: **one plan, two operations**. They do not include playing
key overlays, hit-error collection, snapshots, publication, or OS syscalls.
Regular cases retain the same populated full field workload; lazer's existing
regular fixture is still zero-filled and is only a regression diagnostic.

### Changes and safety

Two fixed costs were removed:

- Two-to-four-operation stages use four-entry scratch arrays rather than
  clearing the executor's 64-entry participant array. The generic implementation
  remains shared with larger stages, including address-overflow accounting,
  failed-batch scalar fallback, per-read errors and fallback deadlines.
- The host-owned structural proof caches both x86 and x64 dependent-pointer-width
  checks. Every reuse still compares every structural byte, including operation
  lengths, references, reserved fields and the session handle. A changed plan
  rebuilds both proofs; aliased header tables never enter the cache. The executor
  selects the proof for the actual session bitness before any native read.

No SDK trust flag, validation exemption, new unsafe code, or native generation
check was introduced or removed. The new test covers both bitnesses, changed
widths, invalid widths and repeated cache hits. Existing batch failure,
overflow, deadline, PID-generation and warmed zero-allocation tests remain.
Production single-stage SYNC plans still need no application clock reads;
dependent-stage coarse deadline checks and public absolute deadlines remain.

### Interleaved Criterion results

```sh
taskset -c 0-7,16-23 nice -n 10 python scripts/bench-compare.py \
  --no-reserve --cpu 7 --rounds 5 --warmup 1 --measurement 2 --samples 40 \
  --binary before=target/m1-pp-evidence/before \
  --binary after=target/m1-pp-evidence/after \
  --filter '^plan_memory/' --output target/m1-pp-evidence/paired
```

All task-owned builds and Rust correctness gates had finished before measurement.
CPU 7 is pinned, **not exclusive**; sibling CPU 23 and other host activity
remain unrestricted. Five invocations per revision interleave before/after.
Values are **median of invocation medians [minimum–maximum]**, in ns:

| `plan_memory/` case | Before | After | Median change |
| --- | ---: | ---: | ---: |
| `stable_precise_native` | 6.209 [6.152–6.759] | 6.197 [6.007–22.940] | −0.18% |
| `stable_precise_plan` | 139.079 [133.679–144.656] | 122.468 [121.294–154.897] | −11.94% |
| `stable_native` | 1517.452 [1390.088–1781.372] | 1407.635 [1319.825–1807.563] | −7.24% |
| `stable_regular_fields` | 3991.782 [3667.003–6878.063] | 3667.092 [3585.521–4061.990] | −8.13% |
| `stable_unfused_plans` | 4088.187 [3767.849–6236.055] | 3761.681 [3722.932–4174.076] | −7.99% |
| `lazer_regular_fields` | 12093.411 [11956.751–18061.446] | 11780.274 [11463.030–12634.608] | −2.59% |
| `floor_one_op` | 103.497 [100.097–106.643] | 105.843 [101.442–125.632] | +2.27% |
| `floor_eighteen_independent_ops` | 332.453 [322.610–361.938] | 363.367 [325.957–503.701] | +9.30% |
| `floor_fifty_six_independent_ops` | 848.811 [834.752–917.807] | 835.003 [819.085–943.015] | −1.63% |

**Every revision comparison is inconclusive** under the script's spread/interval
rule; all invocations, including outliers, are retained. The precise plan/native
ratio of aggregate medians is **22.40× before, 19.76× after**, not a within-10%
pass. The native fixture is two in-process slice copies, approximately 6 ns,
whereas the plan still serializes, executes, validates and returns an ABI message.
Regular plan/native ratios are **2.63× before, 2.61× after**. No regular
regression is established; neither these ratios nor the noisy point-estimate
speedup proves the live D17 exit bar.

### Real-process host diagnostic

The normal, profiling-disabled `plan_process_bench` also ran on CPU 7 with
`--pid self --cycles 100000`: five invocations per revision, alternating
revision order each round. Within each invocation native/plan order alternates
every iteration after 100 warmups. This uses real generation-checked Linux
reads, real `PluginHost` ownership/liveness checks and SDK interception. Widths
repeat one readable address, not a populated osu! pointer graph.

```sh
taskset -c 7 nice -n 10 target/m1-pp-evidence/host-after \
  --pid self --cycles 100000
```

Nanoseconds, **median of invocation medians [minimum–maximum]**:

| Shape | Before native | Before plan | After native | After plan |
| --- | ---: | ---: | ---: | ---: |
| One op | 892 [711–942] | 942 [742–1002] | 942 [712–962] | 1012 [742–1012] |
| Precise two ops | 972 [762–1002] | 1072 [841–1102] | 1041 [772–1062] | 1133 [842–1172] |
| Eighteen ops | 832 [821–1123] | 1072 [1062–1453] | 1102 [831–1152] | 1433 [1072–1503] |
| Eight stages / 43 ops | 7514 [5751–7745] | 8005 [5992–8225] | 7565 [5852–7815] | 8005 [6112–8296] |

Paired precise plan/native overhead, in invocation order:
**before +10.29%, +11.09%, +9.98%, +10.37%, +10.36%**;
**after +9.51%, +9.09%, +8.84%, +10.36%, +9.07%**.
The paired median moves **+10.36% → +9.09%**, but one candidate invocation
still exceeds 10%. Absolute timings drift substantially with the native
control too; this is a narrowly favorable host-model point estimate, **not a
confidence-qualified D17 pass**. The other shapes' paired median overheads
are one-op **+5.61% → +6.41%**, eighteen-op **+29.35% → +29.77%**, and
eight-stage **+6.20% → +5.82%**.

Separate profiled invocations (not throughput evidence) report two-op phase
means, before → after: SDK encode **36 → 27 ns**, session resolution
**26 → 27 ns**, validation **52 → 47 ns**, root resolution **22 → 22 ns**,
result delivery **55 → 44 ns**, backend **875 → 875 ns**.
These noisy means include profiler overhead and are not additive with whole-call
medians; unchanged SDK/delivery code also varies. Both revisions report
**zero application precise/coarse clock reads** for single-stage shapes;
eight-stage shapes retain eight coarse reads. Profiler-only reads are
14 per single-stage plan and 28 per eight-stage plan. The paired two-op
plan-minus-native median is **100 → 92 ns**: the residual work is principally
the ABI path around an unchanged checked native read, not an extra health syscall.

### Profile-backed remaining floor

The baseline Criterion executable was separately rebuilt with
`CARGO_PROFILE_RELEASE_STRIP=none` for symbolization only; throughput measurements
above use the normal stripped release profile. A 15-second precise-plan sample:

```sh
taskset -c 7 nice -n 10 perf record -e cycles:u -F 997 \
  --call-graph dwarf --user-callchains -o precise-before.perf.data -- \
  target/m1-pp-evidence/perf-before --bench \
  '^plan_memory/stable_precise_plan$' --profile-time 15
perf report --stdio --no-children -i precise-before.perf.data --sort symbol
```

14,618 samples, zero lost: self shares are **31.21% `PlanMemory::read_flat`**,
**24.15% raw execution**, **17.88% `PlanProcessMemory::read_many`**,
**4.12% structural proof lookup**, **1.52% SDK interception**, and **6.42%
`__vdso_clock_gettime`**. Inlining folds encoding, result validation and copy
work into these symbols; these are not separately measured phase shares.
This executor-only benchmark deliberately calls public `execute_into` with an
absolute deadline and excludes real `PluginHost` routing. Its clock share is
therefore **not** evidence of renewed production SYNC clock overhead.

The remaining copy-only-fixture gap is an ABI round-trip floor, not an
allocation or OS-read regression. Closing a roughly 116 ns gap against a
6 ns native fixture would require avoiding most of the encoding/result delivery
path, for example a prepared, host-validated executable plan and a scoped
caller-destination API with equally strong ownership, bounds and result checks.
Simply skipping validation or the generation check is not an acceptable fix.
No such ABI redesign, native Windows result, VM result or live-daemon CPU
claim is implied here. Remeasure live stable and lazer with the
[native/plan commands above](#live-reader-native-versus-plan), in both orders,
and retain error counts and start/end state.

Raw Criterion logs, bootstrap intervals, profiler data and preserved executables
are local ignored evidence under `target/m1-pp-evidence/`; the paired summary is
`target/m1-pp-evidence/paired/summary.json`.

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

**Raw lazer fixture: binary placement, not added reader work.** The release
table retains the observed **+8.50%** (9.871–9.890 µs versus
10.536–10.900 µs); it was not hidden by changing the fixture. This
zero-filled case repeatedly fails root validation before reaching skin or
renderer reads. Existing profiles locate its work in fixture copying and
AVX2 signature scanning; the untimed trace counted six scans on both sides.

Follow-up attribution on 2026-10-25 ran only
`raw_snapshot_reads/lazer_regular_fields`, with independent worktrees,
target/ Criterion directories, three alternating rounds, `taskset -c 15`,
2 s warm-up, 20 s measurement and 60 samples. No VM or CPU/IRQ isolation
was used. Values are median arithmetic means and their three-round ranges:

| Revision / change | Mean (µs) | Range (µs) |
| --- | ---: | ---: |
| `c791512` — 1.1.0 | 9.919 | 9.894–10.177 |
| `9eb9196` — current skin | 9.844 | 9.830–10.457 |
| `771668e` — renderer state | 9.861 | 9.838–10.375 |
| `8fb8a6a` — atomic selection publication | 9.948 | 9.794–10.135 |
| `b13a910` — combo-colour normalisation | 9.839 | 9.814–9.867 |
| `77080cd` — release source | 10.754 | 10.421–10.921 |

None of the four reader-feature commits adds the step: their medians are
within 0.8% of 1.1.0. Local bisection identifies **`a44183b`**, the unrelated
subscription/settings optimization, after `f170957`. A fresh three-round
pair measured `f170957` at 10.031 µs (9.897–10.424), versus `a44183b` at
10.612 µs (10.480–11.000), **+5.79%**. Reader, process-scanner and benchmark
sources are unchanged across that commit.

The AVX2 finder's first 1,820 hot instruction bytes are identical, but its
entry moves from cache-line offset 0 in `f170957` to 32 in `a44183b` and
`77080cd`. The requested independent control builds used
`RUSTFLAGS="-C llvm-args=-align-all-functions=6"` **for measurement only**.
Both finder entries then align to 64 bytes: `b13a910` measured 9.884 µs
(9.876–9.908), and `77080cd` 9.863 µs (9.799–9.911), **-0.21%**, with
overlapping ranges. The gap collapses when placement is controlled.
This attributes the synthetic failed-root difference to code placement,
not a product reader regression or the cost of reading new fields.
No production flags, padding, reader code or fixture bytes were changed;
this does not claim that new fields are free in populated live captures.

Artifacts are under `~/.cache/vosu-agents/PerfRegressions/raw-lazer/`:
`summary.json`, `placement-results.json`, `finder-layout.json`, and raw
per-round logs/ Criterion JSON. The original six-revision run's third round
resumed after a runner interruption; timestamps are retained. The bounded
placement controls recorded load rather than waiting indefinitely for a
quiet host. The populated deterministic reader's apparent 5.28–9.71% gains
still have overlapping ranges; do not claim resolved improvements.

Raw per-round Criterion JSON/stdout, deterministic-reader stdout,
environment/load records, `summary.json` and full `table.txt` are under
`/tmp/vosu-r4-evidence/Bench120-final/`. This run is taskset-only and does
not reuse the earlier isolated run's raw results.

## M0 build-time goal (section 8 / D9)

Measured **2026-11-22**, pre-M0 baseline `3a91ff9` (via `ovnfs new --rev
3a91ff9` from `/home/user/gh/vecosystem/vosu`) versus M0 crate-split `cd81696`
(`m0/split`). Ryzen 9 9950X3D **host**, kernel `7.3.5-2-cachyos-bore`, Rust
1.96.1, performance governor, boost enabled, default features, `--locked`;
both sides built with an independent `CARGO_TARGET_DIR` (so neither reuses
the other's incremental artifacts) and warmed with one untimed
`cargo build --locked --workspace --all-features` before timing. All
`cargo check`/`build`/`test --no-run` invocations ran under
`taskset --cpu-list 0-7,16-23` (the V-cache CCD; the Windows VM's QEMU
workers are confined to 8-15/24-31) and only while
`pgrep -f scripts/prepush.py` was empty on the shared host.

```sh
python scripts/measure-build.py --runs 5 --json
```

`scripts/measure-build.py` appends a comment line to each probe file, times
`cargo check --locked`, `cargo build --locked`, and
`cargo test --locked -p <crate> --lib --no-run`, reverts, and repeats for 5
runs; it does not take a baseline-path argument, so the baseline run used a
copy of the script placed inside the `ovnfs` checkout (`ROOT` resolves from
`__file__`). `vosu-osu-reader` and `vosu-osu-state` do not exist before M0,
so the baseline measured only `api-fixed`, `server-routes`, and
`dashboard-render`; the script reports this with a `Skipping …` line rather
than failing. Values are medians and min-max ranges of 5 runs per command;
one run in both `api-fixed` series hit a much larger outlier
(`src/api/fixed.rs` only, reproduced in both the baseline and candidate 5-run
sets) — medians are unaffected and are what the verdict below uses.

| Probe (crate) | Command | Pre-M0 `3a91ff9` median (range) | M0 `cd81696` median (range) | D9/section 8 budget | Verdict |
| --- | --- | ---: | ---: | ---: | --- |
| `api-fixed` (`vosu`, root) | `cargo check` | 1.669 s (1.648–15.818 s) | 0.964 s (0.960–6.419 s) | n/a (root-crate probe, cumulative compare) | -42% |
| `api-fixed` (`vosu`, root) | `cargo build` | 2.852 s (2.842–16.651 s) | 1.962 s (1.921–12.593 s) | n/a | -31% |
| `api-fixed` (`vosu`, root) | `cargo test -p vosu --lib --no-run` | 3.858 s (3.821–28.213 s) | 2.461 s (2.425–16.391 s) | n/a | -36% |
| `server-routes` (`vosu`, root) | `cargo check` | 1.664 s (1.646–1.708 s) | 0.969 s (0.953–1.051 s) | n/a | -42% |
| `server-routes` (`vosu`, root) | `cargo build` | 2.842 s (2.801–3.195 s) | 2.000 s (1.926–2.097 s) | n/a | -30% |
| `server-routes` (`vosu`, root) | `cargo test -p vosu --lib --no-run` | 3.844 s (3.838–3.882 s) | 2.467 s (2.442–2.930 s) | n/a | -36% |
| `dashboard-render` (`vosu`, root) | `cargo check` | 1.655 s (1.633–1.710 s) | 0.967 s (0.956–0.978 s) | n/a | -42% |
| `dashboard-render` (`vosu`, root) | `cargo build` | 2.854 s (2.840–2.870 s) | 1.927 s (1.909–1.970 s) | n/a | -32% |
| `dashboard-render` (`vosu`, root) | `cargo test -p vosu --lib --no-run` | 3.828 s (3.794–3.841 s) | 2.477 s (2.432–2.493 s) | n/a | -35% |
| `osu-reader-lazer` (`vosu-osu-reader`, leaf) | `cargo check` | n/a (crate did not exist) | 1.306 s (1.296–1.314 s) | ≤ 2.0 s | met |
| `osu-reader-lazer` (`vosu-osu-reader`, leaf) | `cargo build` | n/a | 2.309 s (2.281–2.329 s) | ≤ 2.5 s (general leaf goal) | met |
| `osu-reader-lazer` (`vosu-osu-reader`, leaf) | `cargo test -p vosu-osu-reader --lib --no-run` | n/a | 0.613 s (0.599–1.091 s) | ≤ 3.0 s | met |
| `osu-state-reducer` (`vosu-osu-state`, leaf) | `cargo check` | n/a | 1.031 s (1.020–1.334 s) | ≤ 1.0 s | **miss, +3%** |
| `osu-state-reducer` (`vosu-osu-state`, leaf) | `cargo build` | n/a | 2.019 s (1.999–2.310 s) | ≤ 2.5 s | met |
| `osu-state-reducer` (`vosu-osu-state`, leaf) | `cargo test -p vosu-osu-state --lib --no-run` | n/a | 0.435 s (0.429–4.694 s) | ≤ 3.0 s | met |

Observations: the root-package probes (`api-fixed`, `server-routes`,
`dashboard-render` — all three stay in `vosu` until later milestones) are
30-42% faster after M0 purely because the root crate shrank once
`vosu-sys`/`vosu-net`/`vosu-core`/the osu! crates were extracted; there is no
explicit D9 budget for them yet (that applies once section 3's mapping
moves them), so this is recorded as the cumulative-cost reference point
section 8 asks for, not a pass/fail line. The new leaf crates meet their
budget except `vosu-osu-state`'s `cargo check`, whose 1.031 s median is
3% over the ≤1.0 s leaf goal (every one of its 5 runs was ≥ 1.020 s, so this
is not a single-run outlier); `vosu-osu-reader`'s own, stricter `cargo
check` goal (≤ 2.0 s) is met with a wide margin (1.306 s). The broader D9
items — full `prepush.py` wall-clock, native Windows Clippy/test/doctest,
clean local/hosted CI builds, and the `cargo test -p <crate>` /
workspace-`--all-features` feature-unification rebuild — were not run in
this session: they need the native Windows gate, and the Windows VM was in
use by a concurrent `prepush.py`/`land.py` run on this host for an unrelated
change during this measurement.

### Remaining D9 items: clean build times, hosted CI, release builds (2026-11-22)

Measured **2026-11-22**, pre-M0 baseline `3a91ff9` versus the final landed M0
commit `8af9277` (`origin/main`), closing the D9 items the measurement above
could not reach. Same Ryzen 9 9950X3D host, Rust 1.96.1, `taskset
--cpu-list 0-7,16-23` (the V-cache CCD); each checkout is an independent
`ovnfs` worktree with its own `CARGO_TARGET_DIR`, recreated empty for every
run (3 runs each, debug and release), so no run reuses another's incremental
artifacts. Release builds: `cargo build --release --locked
--no-default-features --features linux-process-events --target
x86_64-unknown-linux-gnu`. Debug builds: `cargo build --locked`.

| Build | Pre-M0 `3a91ff9` median (range) | M0 `8af9277` median (range) | Change |
| --- | ---: | ---: | ---: |
| `cargo build --locked` (debug, clean target) | 28.41 s (28.37–29.23 s) | 24.84 s (24.67–25.53 s) | -13% |
| Release build (clean target) | 121.17 s (119.86–121.47 s) | 74.53 s (74.23–75.52 s) | -38% |
| Release binary size (`target/.../release/vosu`, Linux) | 84,455,784 B | 84,828,584 B | +0.44% |

Hosted CI (`gh run view --json jobs`, `ci.yml` on `main`, push event): `3a91ff9`
run [44985148813](https://github.com/18-lab-dev/vosu/actions/runs/44985148813)
(exact Restore Rust artifacts cache hit), `8af9277` run
[45004177770](https://github.com/18-lab-dev/vosu/actions/runs/45004177770),
attempt 2 (also an exact cache hit). Both are the `Windows MSVC native
verification` job only; this is the job the two paragraphs below correct.

| Windows MSVC native verification step | Pre-M0 `3a91ff9` | M0 `8af9277` | Change |
| --- | ---: | ---: | ---: |
| Restore Rust artifacts | 51 s | 124 s | +73 s |
| Lint native Windows targets and features | 51 s | 57 s | +6 s |
| Test native Windows workspace | 337 s | 440 s | +103 s |
| Job total | 495 s | 681 s | +186 s |

**Correction (2026-11-22, same day):** an earlier revision of this section
called the hosted Windows gap "hosted-runner variance" against a different,
non-cache-hit `8af9277` run (job total 1303 s) and a same-host
`scripts/prepush.py` native-phase comparison that is not the hosted job. Both
commits in the cache-hit table above restored the Rust-artifacts cache
exactly, so the +186 s is not a cache miss or queueing artifact. It is a real
regression: M0 added ~8 workspace crates without the Ubuntu validation job's
`CARGO_PROFILE_DEV_DEBUG: line-tables-only`, so `windows-native` built full
PDB debuginfo for every one of those crates' test binaries, growing the
cached `target` directory (bigger Restore step) and the per-binary link/PDB
cost (bigger Test step). The fix and its native VM verification follow.

### Native VM fix: line-tables-only debuginfo (2026-11-22)

Measured natively in the vosu-windows-pro VM (Rust 1.96.1, MSVC, same
toolchain as the hosted runner), simulating the hosted job's cache-hit case:
for each commit and candidate, `cargo test --workspace --lib --bins --tests
--examples --all-features --locked --target x86_64-pc-windows-msvc` (ci.yml's
exact `Test native Windows workspace` command) ran once to populate
`CARGO_TARGET_DIR` with dependency artifacts, then `cargo clean -p` on every
workspace member (dirtying only the workspace's own build outputs, as a fresh
checkout onto a restored cache does) before each of two timed repeats.
`CARGO_BUILD_JOBS=4` throughout, matching the public-repo hosted runner's 4
vCPUs (this VM otherwise has 16); the hosted runner's own number is not
independently confirmed, only GitHub's documented standard-runner spec.
Candidates: current settings; `CARGO_PROFILE_DEV_DEBUG=line-tables-only`
(`lt`); that plus `CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER` pointed at
LLVM's `lld-link.exe` (`lt-lld`). Values are medians of two runs; the
workspace test suite includes timing/network-sensitive cases, so individual
runs ranged roughly ±15-20% around the median (one `3a91ff9`/current-settings
run additionally hit an unrelated flaky test failure, included below since it
still ran to completion).

| Commit | Candidate | Median (two runs) | Range | `target` dir after |
| --- | --- | ---: | ---: | ---: |
| `3a91ff9` | current settings | 222.3 s | 161.9-282.7 s | 5.26 GB |
| `3a91ff9` | `lt` | 227.6 s | 189.0-266.2 s | 2.48 GB |
| `3a91ff9` | `lt-lld` | 182.2 s | 178.4-185.9 s | 2.49 GB |
| `8af9277` | current settings | 241.0 s | 236.0-246.1 s | 5.43 GB |
| `8af9277` | `lt` | 222.7 s | 220.4-225.1 s | 2.65 GB |
| `8af9277` | `lt-lld` | 227.3 s | 225.3-229.4 s | 2.66 GB |

`lt` is the fix applied to `ci.yml`'s `windows-native` job and mirrored in
`scripts/native-ci.ps1`. Within `8af9277` it is paired-faster than current
settings (241.0 s -> 222.7 s, -7.6%); within `3a91ff9` the two-run medians
went the other way (222.3 s -> 227.6 s), inside that commit's own 161.9-282.7
s current-settings spread, so that single comparison is noise, not a real
regression from the setting. `lt` also roughly halves the cached `target`
directory on both commits (`8af9277` 5.43 GB -> 2.65 GB), which is what the
hosted Restore Rust artifacts step actually pays for. That brings
`8af9277`'s own time down to within 0.4 s of `3a91ff9`'s noisy
current-settings median: effectively parity given the ±15-20% run-to-run
spread the full `cargo test` command carries from its own test suite,
independent of this debuginfo setting.
`lt-lld` added no further build-time benefit over `lt` alone in either commit
(and introduces a VM-specific LLVM path the hosted runner does not
provision), so only `lt` was applied. The cache key gained an `-lt-`
generation (`scripts/native-ci.ps1`'s own comment and the matching one in
`ci.yml` record why) so the job does not restore an incompatible
full-debuginfo `target` directory under the new profile.

This does not fully explain the hosted Test step's absolute magnitude (local
VM runs land at 160-280 s against the hosted step's 337-440 s); the
remainder is consistent with slower per-process overhead on an
un-tuned hosted runner (this VM has Windows Defender exclusions on its
cargo directories that a hosted runner does not) rather than anything
`CARGO_PROFILE_DEV_DEBUG` or the linker choice can address.

## Local dev build speed-up tools: sccache and mold

Measured **2026-11-23** on `7ea1c3d`, in a dedicated `ovnfs` checkout
(`dl-buildspeed`) of this repository. Ryzen 9 9950X3D host, kernel
`7.3.5-2-cachyos-bore`, Rust 1.96.1, performance governor, `--locked`,
`taskset --cpu-list 0-7,16-23` (the Windows VM's QEMU workers are confined
to 8-15/24-31; measurements only ran while the VM was otherwise idle). Tools:
sccache 0.11.0 (`cargo install --locked sccache --version 0.11.0`, built with
the `vendored-openssl` feature because the host's system OpenSSL is newer
than `openssl-sys` supports) and mold 3.0.0, extracted from the official
`x86_64-linux` release tarball into a user-local path (no root available for
`pacman -S mold` non-interactively: `sudo -n true` failed with "a password is
required"). `rust-lld` was represented by the system `lld`/`ld.lld` package
already installed (same LLVM lld implementation Rust's bundled `rust-lld`
is), driven through `-C link-arg=-fuse-ld=lld`. Each configuration used its
own `CARGO_TARGET_DIR`; sccache used its own cache directory, separate from
every target dir.

### Cold new-checkout cost (sccache)

Models a new `ovnfs` checkout (or a second coding agent) building the same
commit: `cargo clippy --workspace --all-targets --all-features --locked --
-D warnings` then `cargo nextest run --workspace --lib --bins --tests
--examples --all-features --locked --no-run`, from an empty `CARGO_TARGET_DIR`
each run (default `CARGO_INCREMENTAL` left at Cargo's default, i.e. not
`prepush.py`'s own `CARGO_INCREMENTAL=0`). The sccache cache was warmed by
one untimed prior build in a different target dir, then 5 further runs each
started from a fresh, empty target dir against that warm cache. Baseline also
ran 5 times, each from a fresh empty target dir, no sccache.

| Configuration | Median total (range) | vs. baseline |
| --- | ---: | ---: |
| No sccache (cold, 5 runs) | 54.800 s (54.652–55.254 s) | — |
| `RUSTC_WRAPPER=sccache`, warm shared cache (5 runs) | 41.208 s (41.148–59.873 s) | **-24.8%** |

The sccache series' first run (59.873 s) is a one-time sccache-server
startup/warm-up outlier (the daemon was freshly started for that series);
the other 4 runs landed in a tight 41.148–41.574 s band, so the median is
representative. Final `sccache --show-stats` for the 5 timed runs: 7,440
compile requests, 71.50% overall hit rate (83.33% for the bundled C/C++
dependencies, 66.67% for Rust); of the 1,539 compilations that still ran,
306 were non-cacheable specifically because of `incremental` compilation —
this repository's own workspace crates (`vosu`, `vosu-core`,
`vosu-osu-reader`, …) use Cargo's default incremental dev profile, which
sccache cannot cache, so only third-party dependencies were served from
cache in this run. `prepush.py` already sets `CARGO_INCREMENTAL=0` for its
own builds (`scripts/prepush.py`'s `CARGO_ENV`), so sccache would cache the
workspace crates too there; that combination was not separately measured to
a clean, reproducible number in this session; an inc0 probe showed large
run-to-run spread attributable to the host's page cache warming over the
session rather than to the setting itself; and was removed from this record
rather than overstate it.

### Incremental edit loop (mold / lld)

Used `scripts/measure-build.py --runs 5` unmodified (its own probes: edit a
marker line into `api-fixed`, `osu-reader-lazer`, `osu-state-reducer`,
`server-routes`, `dashboard-render`; time `cargo check`, `cargo build`,
`cargo test -p <crate> --lib --no-run`; revert; repeat). `cargo check` never
links, so it is unaffected by the linker and serves as a methodology check
(its deltas were all within ±2.3%, i.e. noise). Sums of the 5 probes' command
medians:

| Commands | Default linker | mold (`-fuse-ld=mold`) | system `lld` (`-fuse-ld=lld`) |
| --- | ---: | ---: | ---: |
| `cargo check` × 5 probes | 5.299 s | 5.331 s (+0.6%) | 5.289 s (-0.2%) |
| `cargo build` + `cargo test --no-run` × 5 probes (link-affected) | 18.657 s | 17.013 s (**-8.8%**) | 18.711 s (+0.3%) |
| All three commands × 5 probes | 23.957 s | 22.344 s (-6.7%) | 24.001 s (+0.2%) |

Per-probe, mold's `cargo build` savings ranged 8.6–14.2% and its
`cargo test --no-run` savings ranged -5.1–+9.6% (`server-routes`'s test
probe was the one regression, within its own run-to-run range); `lld`'s
deltas on every command, every probe, stayed within ±2.1%, indistinguishable
from noise. mold's link-step win is real (confirmed by `readelf -p .comment`
showing `mold 3.0.0` vs. `LLD 22.1.2` in the two linked test binaries) but
does not reach 15% once folded into the whole check+build+test loop, because
linking is only part of each command's cost for these small probe crates.

### Representative agent iteration (vosu-core)

Edit `crates/vosu-core/src/lib.rs` (append a marker line), build its test
binary (`cargo nextest run --locked -p vosu-core --lib --no-run`), run it
(`cargo nextest run --locked -p vosu-core --lib`), revert; 5 cycles per
configuration against a target dir warmed by one untimed full-workspace
`cargo nextest run --no-run`. The first cycle of each series re-pays
nextest's one-time list-building cost (~10 s) and is excluded as a known,
reproduced-on-both-sides artifact; medians below are of cycles 2–5:

| Configuration | Build median | Run median | Total median |
| --- | ---: | ---: | ---: |
| Default linker | 0.673 s | 0.550 s | 1.213 s |
| mold | 0.586 s (**-12.9%**) | 0.548 s (-0.4%) | 1.134 s (**-6.5%**) |

`vosu-core` is a leaf crate with a small test binary, so even mold's ~13%
link-time win is a small absolute amount against the test-run time it cannot
affect; the realistic edit-loop total falls short of the 15% bar.

### Decision

- **Adopt sccache**, opt-in only, via `source scripts/dev-env.sh`
  (`CONTRIBUTING.md` "Speed up the local build loop"). It clears the 15% bar
  by a wide margin (-24.8%) on the cold-checkout cost that dominates when
  several `ovnfs` checkouts or coding agents build the same commit, with no
  change needed to `prepush.py` or `measure-build.py` (both already inherit
  the full process environment, so an exported `RUSTC_WRAPPER` reaches their
  subprocesses unmodified). No correctness risk: it is a transparent,
  content-addressed object cache around `rustc`, never set by CI or any
  committed config, and release builds are untouched (only exported in an
  interactive shell via `dev-env.sh`).
- **Do not adopt mold or lld.** mold is a genuine, reproducible win on pure
  link time (9–14% per `cargo build`/`test --no-run`) but the realistic
  edit-check-build-test loop and the `vosu-core` iteration both land at
  6.5–8.8%, short of the 15% bar; system `lld` showed no measurable benefit
  over the default linker at all. Neither is wired into any build path;
  `scripts/dev-env.sh` sets no linker variables.

## M1 PlanMemory (paired, D17)

### Decision and final source

**D17 is not a pass.** The owner extends the M1 L3 Windows exception to every
missed row and permits v2/main landing. D17 remains a **hard release gate**,
re-measured at **M4a X1 and M5**, with Main accountable. Final
measured source: **`1162fe6de744b3361eaa3f097234e76e9c9b8162`**, 2026-11-24.
Consume the complete `b741219..m1/wp-fix` branch range, not just its final commit.
The maintainer's `win-osu/evidence/wp-fix/REPORT.md`, `prefix-raw/` and
`prefix-summary.json` retain the per-row decision, every sample, executable/source
hashes, Criterion confidence intervals and a ten-line draft exception.

The threshold remains **no median regression above 10%**, with D17's pinned-CPU
Criterion confidence qualification. A paired point estimate within 10% is not
a confidence-qualified pass when the interval is inconclusive.

### Protocol

Windows 11 Pro, native Rust 1.96.1/MSVC, corrected approximately 19 ns QPC,
normal `--release --locked --no-default-features`, no temporary profiler or
allocation counters. Live captures use the same menu-state PID per client,
affinity mask 4, **native/plan/plan/native**, 1,000 warmups and nine samples of
10,000 captures. Pair rounds 1/2 and 4/3. Strict untimed readiness requires a
full-length zero-error capture before the accepted sequence.

All eight final live invocations remained `0 -> 0` with zero regular and precise
errors. Both clients were stopped afterward; the VM remained running. No viewer,
client GUI interaction, VM reboot or shutdown was used.

Criterion uses `--bench`, 1 s warmup, 2 s measurement, 30 samples and separate
invocation directories. Stable populated fixtures run native/flat/flat/native;
lazer precise runs native/flat/routed/routed/flat/native. Windows fixture reads
use native self-process RPM, with successful equality/assertion checks before
timing. Stable regular covers global/menu/gameplay fields. The zero-filled
lazer regular diagnostic is not a populated acceptance fixture.

### Final fixture verdicts

Times are µs. Each native/plan cell retains both invocation medians in pairing
order; percentages are the two paired overheads and their mean.

| Fixture / route | Native | Plan | Paired overheads | Mean | Verdict |
|---|---:|---:|---:|---:|---|
| stable regular, flat | 14.186 / 14.091 | 16.633 / 17.159 | +17.25% / +21.77% | +19.51% | Miss |
| stable precise, flat | 0.752 / 0.721 | 0.978 / 0.948 | +30.08% / +31.53% | +30.81% | Miss |
| lazer regular | — | — | — | — | Not established: no populated fixture |
| lazer precise, flat | 14.956 / 13.384 | 16.167 / 15.046 | +8.10% / +12.42% | +10.26% | Miss |
| lazer precise, routed | 14.956 / 13.384 | 16.706 / 15.459 | +11.70% / +15.51% | +13.61% | Miss |

Stable precise's two-pair 95% interval is +21.58..+40.04%; its individual
Criterion intervals also separate beyond the threshold. The other paired
fixture intervals are wide and retained in the report. Lazer precise ratios
of mean invocation medians are +10.14% flat and +13.50% routed; these differ
from means of paired percentages.

### Final live verdicts

Intervals below use independent invocation pairs (Student-t, df=1), not the
nine correlated samples within each invocation as independent replicates.

| Capture | Native µs | Routed plan µs | Paired overheads | Mean (95% interval) | Plans/capture | Verdict |
|---|---:|---:|---:|---:|---:|---|
| stable regular | 99.693 / 96.789 | 124.164 / 115.745 | +24.55% / +19.58% | +22.07% (-9.46..+53.59%) | 16 | Miss |
| stable precise | 3.673 / 2.970 | 3.601 / 3.465 | -1.96% / +16.69% | +7.36% (-111.14..+125.87%) | 1 | Point pass only |
| lazer regular | 203.780 / 181.186 | 215.284 / 232.959 | +5.64% / +28.57% | +17.11% (-128.56..+162.78%) | 87 | Miss |
| lazer precise | 7.406 / 7.602 | 10.112 / 9.554 | +36.53% / +25.68% | +31.11% (-37.83..+100.05%) | 3 | Miss |

These are transport comparisons, not whole-daemon CPU/tick, read-to-publication
latency or a new comparison against 1.x `6ffc087`.

### Windows complete-graph attribution

`plan_floor` records the active lazer graph once and replays prebuilt requests
without adapter construction/reader decoding. The final native replay preserves
scalar/batch boundaries and native coalescing: **8 submissions, 51 ops, 1,840
requested bytes, 1,808 submission bytes and 3,192 result bytes**.

Means of two normal-release invocation medians:

| Component | ns |
|---|---:|
| Native, matched batching | 12477.45 |
| Validating executor | 13354.57 |
| SDK encode + synchronous submission setup | 97.33 |
| Result validation | 45.56 |
| Warmed result-buffer copy | 27.83 |
| Prebuilt flat SDK replay | 15510.98 |
| Full PluginHost replay | 16022.92 |

Executor minus matched native is **877.12 ns**. Isolated framing, validation
and ownership-copy operations are required at the M1 boundary, but their
durations are not claimed optimal or an irreducible timing floor. The remaining
flat SDK/delivery/access delta after those probes is **1,985.70 ns**; routing's
descriptive delta is **511.94 ns**. Both require further isolation.

The full populated-capture routed/native gap is **1,912.87 ns**, versus
**3,545.47 ns** in graph replay: the **-1,632.60 ns** reconciliation and second
flat graph invocation's drift prohibit an additive causal/frozen-ABI floor
claim. Do not clamp the residual or transplant the active fixture's costs to
three-plan menu captures.

Every stable fixture/live and lazer live miss has its complete measured gap
explicitly marked **unattributed, assigned to Main / M4a X1** in the report.
Main (the vosu 2.0 orchestrator) owns that end-to-end attribution and the missing
populated lazer regular fixture. Main owns the **M4a X1 re-measurement** and
**M5 process-runtime D17 re-check as a release exit gate**: repeat the populated
fixtures and all four live rows with pinned, zero-error paired captures and the
unchanged confidence rule;
close the >10% rows or obtain a fresh explicit exception.

### Correctness, reductions and rejected evidence

Windows module snapshots now retry only errors 24/299, only with module flags,
for at most eight attempts/seven yields, preserving the final error.
Injectable policy tests cover success, both codes, excluded flags/errors and
exhaustion. Origin/main `a92c30483c90` has the same one-shot latent bug and was
not changed. The host retains 16 exact-byte validated shapes, reuses the
previous-stage result prefix for immediate dependencies and permits inlining
of validated SDK result access/application. ABI layout, validation, error
ordering and owner/generation/liveness rules remain intact.

Final code passed 773 Linux core/osu/reader/SDK tests, both plan allocation and
full-field invariants, Linux/Windows-target warning-denying Clippy and all nine
maintained check scripts. Native vm-tests passed: sys 48 (1 ignored), core
read-plan 61, SDK validation 5, adapter memory 26 and reader memory 155.
The first native reader run exposed a hardcoded Linux read-count expectation:
Windows menu prefetch lets later connection consumers reuse the cached value
even with the scalar Probe. The test now bounds counts by platform while
retaining all value and injected-failure parity assertions. All 155 native
reader tests and 191 Linux reader tests passed after this test-only correction.
No production path/read graph changed; **1162fe6 timings remain valid**.
Exact commands, the original failure and corrected receipts are in the report.

The original fusion attach error 24 produced no accepted live timing.
Candidate `80f5e2d` stable native regular had two errors and is rejected,
not averaged into the final run. The earlier `66d3b35` uncoalesced graph-native
replay is not a valid floor comparator. Failed diagnostic builds, source-asset
staging and test-selection attempts remain preserved beside corrected receipts.

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
[verification history](verification-history.md#session-clock-and-cadence-remediation).
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

## Selected-field projection cost

Reproduce the maintained synthetic regression cases with:

```sh
cargo bench --locked --package vosu --bench hot_paths -- v2_subscription_document
```

The cases cover keys/time, complete histories, individual history indices, and
whole-document selection, with empty and 7,500-entry histories. Whole-document
iterations use fresh snapshots so their schema-encoding cache cannot hide work.

A separate **2026-10-27** Linux process-CPU experiment used the same optimized
1.2.1-derived binary, switching only between its former full-document projection
algorithm and selection-pruned projection. The unchanged recorded state contained
7,488 combined, 5,042 press, and 2,446 release errors. Twelve clients shared three
plans: keys/time/context, visualizer settings/context, and a deliberately expensive
complete combined-history selector. Publications were requested every 1 ms.
Three alternating-order rounds measured three seconds per case, pinned to CPU 15,
with private HOME/XDG directories. Values below are medians; CPU is percent of one
core, measured from the child process's own user+system time.

| Histories | Full-document CPU % | Selected CPU % | Full µs/document | Selected µs/document |
| --- | ---: | ---: | ---: | ---: |
| Recorded | 70.347 | 42.915 | 719.709 | 440.719 |
| Empty control | 4.046 | 2.895 | 40.987 | 29.483 |

This removes **39.0%** of process CPU in the recorded-history case (**38.8%** per
document), despite the intentional full-history consumer. Every run sent exactly
12 initial frames and no updates; both algorithms sent 103,516 total bytes with
recorded histories and 4,744 with empty histories. Thus the saving is publication
work even when clients' selected values do not change, not reduced delivery.
Differential tests compare actual WebSocket initial/update/not-ready/error/close
frames, including overlay selectors, and randomized pointer unions byte-for-byte.

The whole-document copy/normalization path already existed in 1.1.0. These results
do **not** establish a 1.2-only regression, explain renderer frame drops, or measure
whole-daemon/native-game improvement. Recorded payloads and standalone collector
source remain local investigation evidence, not repository fixtures.

## Controlled Windows overlay comparison (2026-10-27)

Published vosu 1.1.0, 1.2.0, and 1.2.1 were compared with identical isolated
overlay copies, settings, ports, and a deterministic osu!stable mania Autoplay
map. The real pages were the user's beatmap renderer overlay and the EZ2ON Key
Visualizer. The renderer retained its 360 fps setting; the visualizer retained
its 60 fps setting. Runs used a separate profile and private daemon HOME/data
directories, not the user's running applications or configuration.

The virtual display advertised 2560x1440 at 180 Hz after applying the installed
Looking Glass IDD's supported refresh setting. This did **not** establish an
180 Hz presentation path: a separate five-second DWM probe measured 118.6 waits/s
(p50/p95/p99 8.539/9.151/9.726 ms), and simple Chrome animation probes varied with
the capture/consumer state. Browser page results therefore describe the
**observed producer cadence**, not a successful 180 fps presentation test.

Two 30-second Chrome 156 page windows per vosu version measured the following
daemon CPU and renderer animation cadence. CPU uses the daemon process's own
user+system time; the raw-endpoint collectors below were not connected during
these page/CPU windows.

| vosu | CPU %, two windows | Renderer rAF/s, two windows | Renderer rAF p50/p95/p99, ms, second window |
| --- | --- | --- | --- |
| 1.1.0 | 14.91 / 15.27 | 127.63 / 127.61 | 7.69 / 8.44 / 8.68 |
| 1.2.0 | 14.25 / 14.05 | 127.55 / 127.52 | 7.80 / 8.57 / 8.68 |
| 1.2.1 | 13.45 / 13.25 | 128.02 / 128.17 | 7.72 / 8.20 / 8.61 |

The visualizer drew 1,799–1,800 distinct timestamps per window, consistent with
its retained 60 fps setting rather than a 180 fps overlay. Its second-window
draw-gap p95/p99 was 23.60/23.80 ms on 1.1.0, 23.70/24.30 ms on 1.2.0, and
23.40/23.70 ms on 1.2.1. No page errors or main-thread long tasks were recorded.

On the actual pages' own sockets, selected-field delivery during those second
windows comprised 3,697 / 3,749 / 3,775 messages, respectively, approximately
467 / 484 / 482 kB per page. The visualizer's interarrival p50/p95/p99 was
7.80/8.80/15.60 ms, 7.80/8.70/15.50 ms, and 7.80/8.50/15.40 ms. The renderer
received its selected stream in a worker, plus a filtered regular V2 socket.
Main-thread visualizer JSON parsing consumed 15.4 / 13.9 / 14.7 ms over 30 seconds;
renderer-worker parsing consumed 20.68 / 19.40 / 20.01 ms. These pages did not
receive the much larger complete precise histories described below.

Separate matched 60–90 second Autoplay phases added raw V2 and precise clients:

| vosu | V2 messages/s | V2 interarrival p50/p95/p99, ms | Precise messages/s | Precise interarrival p50/p95/p99, ms |
| --- | ---: | --- | ---: | --- |
| 1.1.0 | 14.43 | 101.28 / 104.18 / 108.47 | 87.44 | 15.55 / 16.35 / 16.85 |
| 1.2.0 | 15.50 | 101.11 / 103.54 / 104.61 | 91.46 | 15.52 / 16.31 / 16.85 |
| 1.2.1 | 14.26 | 101.25 / 103.65 / 108.43 | 100.91 | 15.46 / 16.21 / 16.74 |

Complete precise traffic was 21.01 / 21.31 / 23.79 MB per 30 seconds. Message
counts include short extra intervals and must not be mistaken for a uniform
polling period. These observations show no worsened 1.2.x producer cadence or
page CPU/parse bottleneck in this controlled stable workload; they do not
invalidate the reported Linux stutter or identify its presentation-layer cause.

The same fixed vosu 1.2.1 backend also fed published vosulay 1.4.0 and 1.5.0,
with the same two overlays in the same mania-bound profile, normal non-editing
mode, and a 180 fps native cap. Both native versions were run from NTFS with
private caches to avoid changing Chromium's application-path sandbox conditions.
Each table row covers one 30-second page capture during the same 20–50 second
Autoplay phase; combo advanced from approximately 450 to 1,230.

| Native host | Runtime CPU, % of one core | Renderer distinct drawing timestamps/s | Visualizer drawing timestamps/s | Positive rAF gaps p50/p95/p99, ms |
| --- | ---: | ---: | ---: | --- |
| vosulay 1.4.0 standalone | 64.14 | 153.80 | 60.00 | 5.60 / 11.10 / 11.20 |
| vosulay 1.5.0 standalone | 67.31 | 153.27 | 60.00 | 5.60 / 11.10 / 11.20 |
| vosulay 1.5.0 protocol tool-mode supervisor | 78.43 | 153.90 | 59.97 | 5.60 / 11.10 / 11.20 |

The tool-mode control used the published executable with its normal protocol,
token, private data/cache directories, and a stdin/stdout supervisor. It did not
exercise catalog installation or the full vosu ToolHost lifecycle.

Runtime CPU includes the native host and its Chromium children, excluding vosu
and the game. These individual CPU windows are not a statistically established
version or tool-mode regression: an additional 1.4.0 run measured 72.17%.
Native rAF observation produced approximately as many repeated zero-gap
timestamps as advancing timestamps. The table excludes repeated timestamps;
counting callbacks as delivered frames would approximately double the rate.
Instrumented draw counts likewise deduplicate GPU/canvas calls at the same rAF
timestamp and are not swapchain presents.

Renderer distinct-draw gap p50/p95/p99 was 7.50/8.30/8.70 ms on the NTFS 1.4.0
run, versus 7.60/8.60/9.00 ms on standalone 1.5.0. Visualizer draw gaps were
15.70/22.40/23.50 ms versus 15.90/22.30/23.60 ms. No page errors or main-thread
long tasks were recorded in these windows. This comparison does not demonstrate
a 1.5.0 producer-pacing regression or an accidental in-game 30 fps editor cap.

Actual game/present pacing remains unmeasured. PresentMon 2.3 and 2.6 initially
reported access denied; an explicitly authorized, temporary highest-privilege
interactive task subsequently completed without producing a game CSV. That task
was deleted. Native tracing retained startup/bridge logs but did not provide
per-frame delivered timestamps. Neither rAF observations nor the game's visible
fps counter are substituted for that missing evidence. In particular, these
Windows stable results cannot establish Linux lazer or Helium presentation
quality, and no frame-drop improvement is claimed for the projection optimization.

## Process-event backend (comm-hint eBPF vs CN_PROC)

Reproduce with the Linux guest VM already running (`scripts/ebpf-verify.py`
builds the host's real `process_events.bpf.o` and release
`process_event_bench`, stages both into the `vosu-gaming` VM, and runs the
interleaved samples itself; it never starts the VM):

```sh
python scripts/ebpf-verify.py --bench 5
```

`process_events.bpf.c`'s bounded-loop comm-hint matcher (kernel 5.8 floor,
`bpf/process_events.bpf.c`) loaded all five programs well under the
100,000-processed-instruction budget on the `vosu-gaming` VM
(linux-zen 7.2.11, `bpftool -d prog loadall`):

| program | processed | limit | total_states | peak_states | verify_us |
| --- | ---: | ---: | ---: | ---: | ---: |
| process_exec | 64,300 | 1,000,000 | 1,586 | 236 | 270,847 |
| process_fork | 101 | 1,000,000 | 9 | 9 | 356 |
| process_exit | 64,446 | 1,000,000 | 1,639 | 237 | 283,781 |
| process_exit_group_dead | 62,437 | 1,000,000 | 1,583 | 234 | 286,635 |
| task_rename | 64,483 | 1,000,000 | 1,595 | 239 | 271,574 |

Worst case (`process_exec`/`task_rename`) is about 64% of the 100,000
budget, 6.4% of the kernel's hard 1,000,000-instruction limit.

Bench samples ran the staged release `process_event_bench` in the guest,
5 interleaved `ebpf`/`cn-proc` samples each with `idle-ms 1000 iterations 16
churn-rate-hz 50 churn-duration-ms 10000`. Lifecycle CPU is the backend's own
(self, not reaped-child) user+system time during the churn phase; median
detect latency is the add-phase median (time from releasing a pre-spawned
target to its PID appearing in the lifecycle snapshot):

| backend | lifecycle CPU (own, µs) mean / range | median detect latency (µs) mean / range |
| --- | ---: | ---: |
| eBPF | 50,085 / 48,827–51,984 | 173.2 / 164–183 |
| cn-proc | 77,558 / 75,317–80,160 | 233.4 / 221–254 |

This is a rerun on this VM at the time of this commit, not FilterFix3's
earlier manual run from the same branch: that run (same bounded-loop design,
same `idle-ms`/`iterations`/churn settings, but `taskset -c 0-3`-pinned and
built with a dedicated `CARGO_TARGET_DIR`) measured eBPF at 51,976 µs
(51,661–53,067) lifecycle CPU and 255.2 µs (232–274) median detect latency,
and cn-proc at 116,100 µs (113,233–117,687) and 272.0 µs (247–298); both
runs agree the eBPF backend's own CPU is far below cn-proc's and neither
shows a detect-latency regression between backends. The difference between
the two runs (cn-proc CPU roughly 78k vs 116k µs) reflects host/VM load and
core-pinning differences between the two invocations, not a code change —
this file keeps only the latest measurement per the policy above.

Synthetic fixture only: an isolated VM guest with a copied target binary and
unrelated `/bin/true` churn, not live-game or whole-daemon load.

