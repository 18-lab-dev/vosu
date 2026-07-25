---
type: Experiment Report
title: Linux process-event experiment
description: Measured comparison of polling, CN_PROC, and eBPF process discovery, including corrected lifecycle behavior and production-readiness gates.
tags: [linux, process-events, ebpf, cn-proc, performance]
---

# Linux process-event experiment

**Dates:** 2026-07-20 host experiment; 2026-07-21 gaming-VM continuation; 2026-07-22 autoresearch continuation  
**Worktree:** `/home/user/gh/vosu-process-events`  
**Branches:** `experiment/linux-process-events`; `autoresearch/continue-the-experiment-continually-ingesting-do-20260722`  
**Decision:** Do not make process events the unconditional production default. Corrected eBPF is the strongest measured and technically preferred transport on explicitly eligible Linux hosts, but it is not production-ready until the identity, metadata-refresh, ordering, deployment, licensing, and kernel-matrix gates below are resolved. CN_PROC remains a no-added-capability but noisy alternative in the tested initial user/PID namespace, and polling remains the portable fallback.

## Executive summary

This experiment compared periodic polling, Linux process connector (`CN_PROC`), and eBPF tracepoints for process discovery. The completed continuation ran all three modes in an Arch Linux gaming guest with equal-duration synthetic workloads and three live osu!lazer samples per mode.

The continuation fixes and supersedes the earlier conclusion that eBPF normally took about five seconds to remove a client. That result was real for the earlier implementation, but it is now **historical and resolved**: an exit event was discarded while pidfd still transiently reported the process alive. The corrected userspace lifecycle keeps exit intent until the runtime's pidfd-backed tracked identity is confirmed dead. The final eBPF design also corrects the initial-target and concurrent-exit defects described below. Its renamed-away initial-scan preflight removed the observed PID in **11.919 ms**.

In the equal-duration synthetic run, every mode used a five-second idle phase followed by a 60-second lifecycle phase: 20 add/rename/remove iterations plus 50 Hz unrelated-process churn (3,000 spawned processes). There were no timeout outcomes or reconciliation requests. Final eBPF observed 100 early-filtered records, 100 notifier wakes, and 58 targeted inspections, compared with CN_PROC's 6,278 total parsed connector records, 3,154 wakes, and 3,095 targeted inspections. CN_PROC recorded 6,277 records in the lifecycle phase plus one in idle. Polling did 285 full scans overall. During the 60-second lifecycle phase, measured benchmark/runtime CPU was 374.091 ms for eBPF, 603.452 ms for CN_PROC, and 503.930 ms for polling. These are one `RUSAGE_SELF` aggregate across the benchmark process's threads and components, not whole-system energy measurements.

The final live comparison ran `/opt/osu/osu-headless`, which execs osu!lazer through headless gamescope because the passed RTX 5090 had no connected display. All six event-backend samples were healthy, every one of the nine samples had controlled target-PID attribution to the benchmark-owned process group, and every launcher survived the five-second dwell. Median removal latency was **154.943 ms eBPF**, **78.865 ms CN_PROC**, and **1,009.574 ms polling**. Median add latency was **253.354 ms**, 290.849 ms, and 304.624 ms respectively, but gamescope/wrapper startup dominates that metric; it must not be compared directly with the earlier direct-launch host add measurements near four milliseconds.

The technical result favors corrected eBPF where its prerequisites are deliberately provided. It does not settle deployment or legal questions. eBPF needs an explicit capability/service and memlock design, loss/reconciliation behavior remains necessary, and the BPF loader/object licensing needs legal review. The BPF `SEC("license")` token is a kernel loader compatibility declaration, **not** a legal license grant. Until those questions are resolved, do not select process events as an unconditional production default.

The 2026-07-22 autoresearch continuation tightened both event paths and repeated the deterministic 50 Hz lifecycle workload. Across the final nine samples per backend, eBPF used 11.4% less total measured workload CPU and 34.3% less benchmark/runtime `RUSAGE_SELF` CPU than CN_PROC. It also delivered 6.8% faster add and 26.7% faster remove medians, but its `comm`-rename median remained 17.6% slower. Thus eBPF now beats CN_PROC on aggregate measured overhead and two of three latency operations; it does not beat CN_PROC on every operation.

## Objective

The experiment tested whether native Linux lifecycle events could improve:

- discovery latency,
- removal latency,
- `/proc` scanning work,
- behavior under unrelated process churn,
- real osu!lazer PID attribution,
- and operational viability relative to polling.

Correctness remained primary. All event modes retain periodic reconciliation because both kernel event transports are lossy and do not provide replay.

## Environments

The observations are machine-specific and are not service-level guarantees.

### Original host phase (historical)

| Component | Value |
| --- | --- |
| Kernel | Linux `7.1.3-1-cachyos-bore` |
| CPU | AMD Ryzen 9 9950X3D |
| Client | Native osu!lazer AppImage at `/opt/osu/osu` |
| Installed client metadata | osu!lazer `2026.624.0-lazer` |

This phase established low-latency event delivery, CN_PROC amplification, AppImage attribution, raw-tracepoint loading, and the now-resolved early pidfd/eBPF exit race. Its unequal run durations and direct AppImage launch are not used for the final CPU or live-add comparison.

### Arch gaming guest continuation

| Component | Value |
| --- | --- |
| Guest OS | Arch Linux |
| Guest kernel | `7.1.4-zen1-1-zen` |
| QEMU | `11.0.2` with KVM/Q35 |
| Guest memory | 24 GiB (`24576` MiB) |
| Guest CPU topology | 1 socket, 4 physical cores, 2 threads/core: 8 vCPUs |
| Passed GPU | NVIDIA GeForce RTX 5090, PCI ID `[10de:2b85]` |
| Passthrough | VFIO PCI functions through IOMMUFD |
| Guest NVIDIA driver | `610.43.03` |
| Live client | `/opt/osu/osu-headless` -> headless gamescope -> `/opt/osu/osu` |

The host logical CPU pairs used for the vCPUs represent four physical cores selected from the host's V-cache CCD; the guest topology presentation alone is not evidence of cache placement. QMP-reported vCPU thread IDs were pinned and continuously verified as follows:

| Guest vCPU | Host logical CPU |
| ---: | ---: |
| 0 | 4 |
| 1 | 20 |
| 2 | 5 |
| 3 | 21 |
| 4 | 6 |
| 5 | 22 |
| 6 | 7 |
| 7 | 23 |

The QEMU main loop and every other QEMU worker thread were confined to host CPUs `8-15,24-31`. The launcher starts the whole QEMU process on that housekeeping mask, pauses the guest, obtains stable vCPU thread IDs with QMP `query-cpus-fast`, applies singleton vCPU affinities, verifies all residual threads, and only then sends QMP `cont`.

The host's NVML was unusable because the loaded NVIDIA kernel module was `610.43.02` while host userland was `610.43.03`. This prevented host-side NVML validation; it does not invalidate passthrough. Inside the guest, `nvidia-smi` successfully identified the passed RTX 5090 using guest driver `610.43.03`.

The GPU had no connected display in this setup. The live benchmark therefore used this wrapper:

```sh
#!/bin/sh
exec /usr/bin/gamescope --backend headless -W 1920 -H 1080 -r 60 -- /opt/osu/osu
```

## Implementation under test

Two benchmark paths use the `linux-process-events` feature:

- [`examples/process_event_bench.rs`](../examples/process_event_bench.rs): controlled initial-scan preflight, idle phase, add/rename/remove iterations, and unrelated churn.
- [`examples/process_event_lazer_bench.rs`](../examples/process_event_lazer_bench.rs): controlled real-client launch, exact process identity attribution, dwell, process-group termination/reap, and exact-PID disappearance.

The event implementations are:

- CN_PROC: nonblocking `NETLINK_CONNECTOR`, compatibility subscription followed by an optional filtered subscription, userspace candidate filtering, bounded per-TGID work, source-loss detection, and periodic reconciliation.
- eBPF: CO-RE raw tracepoints for process-leader fork (generation maintenance only), exec, rename, and final thread-group exit; candidate filtering in BPF; bounded userspace delivery; source-loss detection; and periodic reconciliation. Linux 6.16+ uses the tracepoint's `group_dead` argument directly, while older kernels retain the `signal->live` fallback. The backend is eligible only in the initial PID namespace.
- Polling: the normal portable `/proc` discovery cadence.

Metric definitions from the JSON records:

- `event_count`: backend records parsed or observed before userspace queue acceptance and coalescing. CN_PROC counts globally delivered connector records parsed in userspace, whereas eBPF counts records only after early in-kernel candidate filtering; these are not identical measurement boundaries.
- `event_wake_count`: Tokio process-event notifier branches taken, not kernel or OS wakeups.
- `full_scan_count`: completed discovery/reconciliation passes.
- `targeted_inspection_count`: per-TGID delta inspections.
- `reconciliation_request_count`: newly latched requests due to loss, parse/truncation, or bounded-queue overflow; periodic five-second scans are excluded.
- CPU and context switches: one `getrusage(RUSAGE_SELF)` aggregate across all threads and components in the benchmark process (benchmark orchestration, Tokio runtime, event backend, and discovery runtime), excluding child-process resource use.

## Procedure and commands

### VM startup and validation

The host launched the guest through the checked systemd service:

```sh
sudo systemctl start vosu-gaming.service
```

Its effective launcher is `/home/user/VMs/vosu-gaming/run-passthrough.sh`. The key QEMU arguments were:

```sh
taskset --cpu-list 8-15,24-31 /usr/bin/qemu-system-x86_64 \
  -machine q35,accel=kvm -cpu host,topoext=on \
  -smp 8,sockets=1,dies=1,cores=4,threads=2 -m 24576 \
  -display none -vga none \
  -object iommufd,id=iommufd0 \
  -device vfio-pci,host=0000:01:00.0,iommufd=iommufd0 \
  -device vfio-pci,host=0000:01:00.1,iommufd=iommufd0
```

The actual launcher additionally supplied OVMF, qcow2, virtio disk/network, QMP, and a root port; the abbreviated command above records the settings material to this experiment. `/home/user/VMs/vosu-gaming/pin-vcpus.py` applied and rechecked the exact affinity map in the environment table before resuming the guest.

Inside the guest, kernel/QEMU/GPU state was recorded with:

```sh
uname -r
qemu-system-x86_64 --version
nvidia-smi
```

### Build and synthetic runs

From the guest checkout:

```sh
cargo build --release --locked --features linux-process-events \
  --example process_event_bench --example process_event_lazer_bench

target/release/examples/process_event_bench poll 5000 20 50 60000 \
  > synthetic-poll.json
target/release/examples/process_event_bench cn-proc 5000 20 50 60000 \
  > synthetic-cn-proc.json
target/release/examples/process_event_bench ebpf 5000 20 50 60000 \
  > synthetic-ebpf-final.json
```

The resulting authoritative records were copied without rewriting to:

- `/home/user/VMs/vosu-gaming/synthetic-poll.json`
- `/home/user/VMs/vosu-gaming/synthetic-cn-proc.json`
- `/home/user/VMs/vosu-gaming/synthetic-ebpf-final.json`

Each run used a five-second idle phase. The lifecycle phase requested 60 seconds, used 20 iterations and absolute 100 ms phase scheduling, and ran `/bin/true` churn at a target 50 Hz. The table reports the measured duration for each mode. Missed churn slots would be skipped rather than extending the run; all three runs spawned 3,000 processes and achieved approximately 49.999 Hz.

### Live runs

The following command shape was executed three times for each mode, changing the output suffix from 1 through 3:

```sh
target/release/examples/process_event_lazer_bench \
  <poll|cn-proc|ebpf> /opt/osu/osu-headless 5000 \
  > live-<mode>-<sample>.json
```

The unchanged CN_PROC and polling records are `/home/user/VMs/vosu-gaming/live-cn-proc-{1,2,3}.json` and `/home/user/VMs/vosu-gaming/live-poll-{1,2,3}.json`. The authoritative final eBPF records are `/home/user/VMs/vosu-gaming/live-ebpf-final-{1,2,3}.json`; they replace the earlier eBPF live files.

For each run the harness:

1. started the real discovery runtime and waited for initial discovery;
2. captured the baseline target set;
3. launched a benchmark-owned process group;
4. required exactly one new published Lazer target;
5. recorded launcher PID, observed target TGID, process group, and Linux start-time ticks as attribution metadata;
6. required the observed target to belong to the owned process group;
7. required the launcher to remain alive for the complete five-second dwell;
8. terminated the process group, reaped the launcher, and timed disappearance of that observed target PID; and
9. checked backend health and metrics.

Because the launcher is headless gamescope rather than direct `/opt/osu/osu`, wrapper startup dominates add latency. The continuation's live add numbers are comparable across its three modes, but **not directly comparable** with the old host's direct-launch add numbers.

## Equal-duration synthetic results

### Lifecycle latency distributions

All values are microseconds. Each distribution contains 20 samples; every synthetic timeout count was zero. The JSON `miss_count` fields alias timeout outcomes rather than recording an independent observation, and were therefore also zero.

| Mode | Operation | Minimum | Median | p95 | Maximum |
| --- | --- | ---: | ---: | ---: | ---: |
| Polling | Add | 54,633 | 69,475 | 82,620 | 83,978 |
| Polling | `comm` rename | 793 | 83,153 | 96,803 | 97,820 |
| Polling | Remove | 999,767 | 1,001,834 | 1,002,071 | 1,002,106 |
| CN_PROC | Add | 190 | 222 | 4,825 | 5,106 |
| CN_PROC | `comm` rename | 120 | 133 | 168 | 171 |
| CN_PROC | Remove | 90 | 106 | 143 | 169 |
| eBPF | Add | 174 | 205 | 350 | 1,001 |
| eBPF | `comm` rename | 175 | 195 | 251 | 3,204 |
| eBPF | Remove | 72 | 85 | 11,188 | 11,279 |

The direct initial-scan preflight starts a copied target before `InstanceRuntime`, requires that exact PID from the initial full scan, renames its leader `comm` to noncandidate `bench-away`, confirms `/proc` observed the rename, then terminates/reaps it and measures observed-PID disappearance. It is excluded from idle and lifecycle metric baselines:

| Mode | Initial target removal |
| --- | ---: |
| Polling | 1,001.812 ms |
| CN_PROC | 0.236 ms |
| eBPF | 11.919 ms |

### Idle metrics: five seconds

| Mode | Actual duration | Events | Event wakes | Reconciliation requests | Full scans | Targeted inspections | User CPU | System CPU | Voluntary CS | Involuntary CS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Polling | 5.000782 s | 0 | 0 | 0 | 49 | 0 | 5.435 ms | 23.623 ms | 99 | 0 |
| CN_PROC | 5.000985 s | 1 | 0 | 0 | 1 | 0 | 0.835 ms | 0.745 ms | 53 | 0 |
| eBPF | 5.001428 s | 0 | 0 | 0 | 1 | 0 | 0.032 ms | 1.536 ms | 54 | 0 |

### Lifecycle metrics: 60 seconds, 20 iterations, 50 Hz churn

| Mode | Actual duration | Spawned / actual rate | Events | Event wakes | Reconciliation requests | Full scans | Targeted inspections | User CPU | System CPU | Total CPU | Voluntary CS | Involuntary CS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Polling | 60.001636 s | 3,000 / 49.9986 Hz | 0 | 0 | 0 | 236 | 0 | 55.945 ms | 447.985 ms | 503.930 ms | 19,296 | 12 |
| CN_PROC | 60.001385 s | 3,000 / 49.9988 Hz | 6,277 | 3,154 | 0 | 11 | 3,095 | 98.486 ms | 504.966 ms | 603.452 ms | 33,806 | 34 |
| eBPF | 60.001140 s | 3,000 / 49.9990 Hz | 100 | 100 | 0 | 11 | 58 | 42.307 ms | 331.784 ms | 374.091 ms | 20,034 | 14 |

Including idle, the JSON totals are 285 scans for polling and 12 scans each for CN_PROC/eBPF. CN_PROC recorded 6,277 lifecycle-phase records plus one idle-phase record, for 6,278 total; final eBPF recorded 100 lifecycle records and none in idle. No mode requested reconciliation. CN_PROC's globally delivered, userspace-parsed connector records and eBPF's early-filtered records do not have identical counting boundaries. `event_count` is still observed records before userspace queue acceptance/coalescing. The equal durations make the single-process `RUSAGE_SELF` CPU aggregate and context-switch counts comparable within this harness, though they remain neither system-wide wakeup counts nor power measurements.

## 2026-07-22 deterministic autoresearch continuation

### Workload and interpretation

`./autoresearch.sh` is the canonical entrypoint. It synchronizes the current source into the existing guest, builds the release `process_event_bench` binary with `linux-process-events`, grants only `CAP_BPF`, `CAP_PERFMON`, and `CAP_SYS_RESOURCE` to that benchmark binary, and executes three CN_PROC samples followed by three eBPF samples. Each sample uses a one-second idle phase and a fixed ten-second lifecycle phase with:

- 16 direct exec add/remove iterations;
- 16 generic-to-candidate `PR_SET_NAME` rename iterations;
- exactly 500 unrelated `/bin/true` processes at 50 Hz;
- zero accepted timeout, reconciliation, or backend-unhealthy outcomes; and
- exact add, rename, remove, and churn sample-count assertions.

The primary metric is the sum of each backend's median lifecycle `RUSAGE_SELF + RUSAGE_CHILDREN` CPU. `RUSAGE_SELF` covers the benchmark, Tokio runtime, event backend, and discovery runtime. `RUSAGE_CHILDREN` covers reaped churn and lifecycle helpers, including BPF execution charged to those tasks. This continuation therefore reports both components rather than hiding process-generation cost. It is a fixed-throughput comparison, not a maximum-throughput search.

The final comparison is the median of autoresearch runs 27, 28, and 30 on the retained tree. Each run already contains three samples per backend, so every final backend column summarizes nine samples.

| Metric | CN_PROC median | eBPF median | eBPF vs CN_PROC |
| --- | ---: | ---: | ---: |
| Fixed churn completion | 500 / 50 Hz | 500 / 50 Hz | equal |
| Total workload CPU | 249.272 ms | 220.863 ms | **11.4% lower** |
| Benchmark/runtime self CPU | 95.416 ms | 62.643 ms | **34.3% lower** |
| Reaped-child CPU | 154.501 ms | 157.688 ms | 2.1% higher |
| Observed backend records | 1,167 | 80 | **93.1% lower** |
| Runtime event wakes | 609 | 79 | **87.0% lower** |
| Targeted inspections | 561 | 47 | **91.6% lower** |
| Total workload voluntary context switches | 6,141 | 4,330 | **29.5% lower** |
| Add latency | 206 us | 192 us | **6.8% lower** |
| `comm`-rename latency | **125 us** | 147 us | 17.6% higher |
| Remove latency | 105 us | **77 us** | **26.7% lower** |

All three final invocations completed their exact workload with healthy backends, zero timeout outcomes, and zero reconciliation requests. Their combined primary metrics were 469,063 us, 472,509 us, and 470,135 us; the 470,135 us median is 5.4% below the single 497,204 us baseline invocation. That baseline comparison is directional because it compares three final invocations with one baseline invocation and because child CPU remains noisy. The direct final eBPF-versus-CN_PROC table is the stronger within-workload result.

### Retained changes

The continuation retained changes only when the deterministic workload passed:

1. Idle polls with no records and no reconciliation request bypass the event counter, queue mutex, and notifier path.
2. CN_PROC same-batch untracked EXIT cancels a pending inspection for the same TGID.
3. The pending event queue is a bounded sorted reusable vector rather than a tree map; it notifies only on an empty-to-nonempty transition and partitions directly into inspect/exit PID vectors during drain.
4. Tracked eBPF map synchronization applies only set differences and preserves partial-success shadow state for retry.
5. Exec clears only candidate state, retaining tracked membership and the impossible-for-live tombstone for the same process generation.
6. `task_rename` reuses its already-computed candidate classification instead of scanning `comm` twice.
7. Linux 6.16+ exit handling reads the tracepoint's `group_dead` argument directly; older kernels retain the CO-RE `signal->live` fallback.
8. Interrupted ring-buffer polls retry within the original timeout instead of immediately marking the backend unhealthy.
9. CN_PROC reuses one bounded 64 KiB receive buffer across polls instead of zero-initializing a fresh stack buffer for every drain.

Measured ideas discarded during the continuation included candidate-state pre-lookup before delete, bypassing required fork generation cleanup, caching the dropped-record map handle, reusing process metadata during root resolution, and four-datagram `recvmmsg` batching. The batching trial kept exact workload behavior but raised CN_PROC self CPU from about 95.5 ms in the retained runs to 103.443 ms, so the single reusable `recvmsg` buffer remains. A packed shared state map was not attempted because userspace tracked-bit updates could race kernel candidate/tombstone updates. The dropped-record array was not memory-mapped because libbpf-rs has no public loaded-map mmap API; a manual atomic mapping would add kernel-capability and lifetime complexity to one counter read per poll without evidence that it is the dominant cost.

### Kernel-source conclusions

- The current CN_PROC per-socket filter was added in Linux 6.6 and filters after producer-side event construction and sequence allocation. It reduces netlink fan-out and userspace work, not all kernel producer cost: [kernel commit 743acf351bae](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/commit/drivers/connector/cn_proc.c?id=743acf351bae1ff7ff4aaadd6a406d4d6091d90b).
- Filtered-out CN_PROC events still consume per-CPU sequence numbers, so sequence gaps cannot be treated as loss. The implementation continues to use socket overflow, truncation, malformed-record, and bounded-queue evidence.
- EXEC cannot be dropped from the CN_PROC subscription: Linux exec changes task `comm` without emitting `PROC_EVENT_COMM`, then emits `PROC_EVENT_EXEC`. Explicit `PR_SET_NAME` uses COMM.
- The BPF ring-buffer reserve/submit path avoids an extra event copy, while the default adaptive wake policy avoids unconditional wakeups: [BPF ring-buffer documentation](https://docs.kernel.org/bpf/ringbuf.html).
- Linux 6.16 added `group_dead` to `sched_process_exit`; the fast path is version-gated and the older `signal->live` path remains for compatibility: [current sched tracepoint definition](https://raw.githubusercontent.com/torvalds/linux/v7.1/include/trace/events/sched.h).
- The reported external claim that eBPF should outperform CN_PROC could not be located in a public primary source during this continuation. The benchmark supports the aggregate-overhead part of that claim on this guest, but not universal superiority: CN_PROC still wins the measured rename latency.

These observations are specific to guest kernel `7.1.4-zen1-1-zen`, this workload, and this machine. They are reproducible evidence, not a cross-kernel performance guarantee.

## Corrected live Lazer results

### Identity and lifecycle attribution

Every sample reported `exact_pid_attribution=true`, meaning the newly published target PID was attributed to the controlled process group. This field does not prove that the harness independently revalidated process generation at removal. `launcher_status_before_termination=null` means the launcher had not exited before the harness terminated it after the dwell.

| Mode | Sample | Launcher PID | Target PID | Process group | Start-time ticks | Launcher survived dwell |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| eBPF | 1 | 17,215 | 17,238 | 17,215 | 377,588 | Yes |
| eBPF | 2 | 17,358 | 17,381 | 17,358 | 378,141 | Yes |
| eBPF | 3 | 17,497 | 17,520 | 17,497 | 378,716 | Yes |
| CN_PROC | 1 | 21,829 | 21,852 | 21,829 | 139,999 | Yes |
| CN_PROC | 2 | 21,982 | 22,005 | 21,982 | 141,137 | Yes |
| CN_PROC | 3 | 22,135 | 22,158 | 22,135 | 142,332 | Yes |
| Polling | 1 | 22,293 | 22,316 | 22,293 | 144,952 | Yes |
| Polling | 2 | 22,447 | 22,470 | 22,447 | 146,202 | Yes |
| Polling | 3 | 22,596 | 22,619 | 22,596 | 147,251 | Yes |

The target differs from the launcher because gamescope is the benchmark launcher and Lazer is its child. Process-group membership establishes controlled attribution of the observed target PID. Start-time ticks were recorded as attribution metadata, but the live harness did not recheck them when timing removal; the table therefore does not independently prove exact-generation removal.

### Raw latency, event, and scan observations

| Mode | Sample | Add | Remove | Events | Event wakes | Recon. requests | Full scans | Targeted inspections | Backend healthy |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| eBPF | 1 | 361.375 ms | 154.900 ms | 3 | 3 | 0 | 1 | 1 | Yes |
| eBPF | 2 | 235.808 ms | 315.516 ms | 3 | 3 | 0 | 1 | 2 | Yes |
| eBPF | 3 | 253.354 ms | 154.943 ms | 3 | 3 | 0 | 1 | 2 | Yes |
| CN_PROC | 1 | 290.849 ms | 78.674 ms | 250 | 48 | 0 | 1 | 35 | Yes |
| CN_PROC | 2 | 261.501 ms | 81.650 ms | 255 | 52 | 0 | 1 | 36 | Yes |
| CN_PROC | 3 | 292.808 ms | 78.865 ms | 255 | 52 | 0 | 1 | 36 | Yes |
| Polling | 1 | 305.433 ms | 1,010.558 ms | 0 | 0 | 0 | 9 | 0 | N/A |
| Polling | 2 | 303.613 ms | 1,007.662 ms | 0 | 0 | 0 | 9 | 0 | N/A |
| Polling | 3 | 304.624 ms | 1,009.574 ms | 0 | 0 | 0 | 9 | 0 | N/A |

Polling's JSON `backend_healthy=false` is expected because polling has no event backend; it is shown as not applicable rather than a polling failure.

### Raw CPU and context-switch observations

These values cover each complete launch, five-second dwell, and removal interval.

| Mode | Sample | User CPU | System CPU | Voluntary CS | Involuntary CS |
| --- | ---: | ---: | ---: | ---: | ---: |
| eBPF | 1 | 1,861 µs | 1,288 µs | 87 | 0 |
| eBPF | 2 | 107 µs | 2,759 µs | 94 | 0 |
| eBPF | 3 | 1,102 µs | 1,846 µs | 87 | 0 |
| CN_PROC | 1 | 1,078 µs | 4,946 µs | 393 | 2 |
| CN_PROC | 2 | 0 µs | 6,114 µs | 403 | 3 |
| CN_PROC | 3 | 2,600 µs | 3,729 µs | 397 | 6 |
| Polling | 1 | 1,373 µs | 5,475 µs | 36 | 0 |
| Polling | 2 | 6,844 µs | 345 µs | 35 | 0 |
| Polling | 3 | 0 µs | 6,886 µs | 36 | 0 |

### Median comparison

| Metric | eBPF | CN_PROC | Polling |
| --- | ---: | ---: | ---: |
| Add latency | **253.354 ms** | **290.849 ms** | **304.624 ms** |
| Remove latency | **154.943 ms** | **78.865 ms** | **1,009.574 ms** |
| Events | **3** | **255** | **0** |
| Event wakes | **3** | **52** | **0** |
| Reconciliation requests | **0** | **0** | **0** |
| Full scans | **1** | **1** | **9** |
| Targeted inspections | **2** | **36** | **0** |
| User CPU | **1,102 µs** | **1,078 µs** | **1,373 µs** |
| System CPU | **1,846 µs** | **4,946 µs** | **5,475 µs** |
| Voluntary context switches | **87** | **397** | **36** |
| Involuntary context switches | **0** | **3** | **0** |

The median CPU fields are medians of each field, not a synthetic “median total CPU” assembled from a different sample. The live intervals differ slightly because removal latency differs. Use the equal-duration synthetic table, not this live table, for the strongest CPU comparison.

## Blockers and resolved failures

### Historical five-second eBPF removal: resolved

The original host samples removed eBPF targets at 4,998.740, 5,003.375, and 5,002.322 ms (median 5,002.322 ms). The tracepoint delivered the exit, but userspace immediately revalidated pidfd liveness. Because `sched_process_exit` can precede pidfd readiness, the event was discarded and the next five-second reconciliation performed removal.

This was an implementation failure, not an inherent eBPF removal limit. Exit intent is now retained and matched to runtime identity until death is confirmed or newer validated state supersedes it. The final gaming-VM eBPF removals were 154.900, 315.516, and 154.943 ms with no reconciliation request and only one full scan per run. Therefore the old five-second conclusion is preserved only as historical evidence of the race and must not be presented as current behavior.

### Initial-target and concurrent-exit eBPF defects: corrected

The initial synthetic helper exists before BPF attachment. The final benchmark strengthens the measured contract by renaming its leader from a candidate name to noncandidate `bench-away` before termination. This exercises and demonstrates the tested userspace-seeded tracked-map path; it is not general proof of every identity or ordering case.

The corrected ownership model is:

- BPF-discovered candidate TGIDs and userspace-seeded tracked TGIDs occupy distinct BPF maps.
- Userspace seeds the tracked map from the initial/full discovery result. Partial update success is shadowed in userspace and retried, so a transient map update cannot silently erase synchronization intent. Any map-synchronization failure conservatively marks the backend unhealthy until restart, even if later retries converge.
- A kernel-owned bounded 128-key LRU tombstone map arbitrates final-group exit. Within the retained tombstone window, the insertion winner alone emits EXIT. Eviction can permit a later harmless duplicate EXIT, so this is bounded suppression rather than an absolute one-EXIT-per-generation guarantee.
- A new process leader's fork/exec clears stale generation state before candidate handling. Fork is generation maintenance only: it deliberately does **not** inspect inherited `comm`.
- Exit checks whether the TGID is a BPF candidate or a userspace-seeded tracked target, then deletes both forms of membership after winning the tombstone gate.

The final renamed-away preflight removed the observed PID in 11.919 ms. An earlier direct `bpf_core_read_str` attempt was verifier-rejected and is not shipped. The authoritative final synthetic eBPF benchmark loaded successfully with zero timeout aliases and zero reconciliation requests. Separately, all three authoritative final live eBPF runs loaded successfully, attributed the controlled PID, completed their dwell/removal procedure, reported zero reconciliation requests, and kept the backend healthy.

### Nondeterministic backend-health observation: mitigated, exact cause unproven

The earlier post-mitigation 60-second run became unhealthy and refused output. The autoresearch continuation reproduced one unhealthy eBPF sample before adding bounded `EINTR` retry to the ring-buffer poll. Temporary error-site diagnostics did not reproduce the failure, so the exact earlier error remains unproven and must not be relabeled as definitively caused by `EINTR`.

After the retry change, ten consecutive benchmark invocations completed with healthy eBPF samples; the three authoritative final invocations account for nine healthy eBPF samples. This is stronger stability evidence and removes a known false-unhealthy path, but it is not a deterministic regression for the original failure.

### CN_PROC amplification: functional, still a concern

CN_PROC opened and stayed healthy without an added capability grant in this tested guest, which ran in the initial user and PID namespaces. Its cost is global traffic and repeated candidate work: the equal-duration lifecycle recorded 6,277 userspace-parsed connector records (6,278 including idle), 3,154 event wakes, 3,095 targeted inspections, and 33,806 voluntary context switches. The live median was 255 records and 36 targeted inspections in a five-second dwell. This is substantially noisier than final eBPF's early-filtered 100/58 synthetic records/inspections and 3/2 live median records/inspections, while recognizing that the two `event_count` boundaries differ.

CN_PROC eligibility still depends on kernel configuration, proc connector support, and execution in the initial user and PID namespaces. It required no added capability in this tested environment; it is not universally capability-free.

### eBPF privilege and deployment model: unresolved for production

The experiment required a controlled loader environment and the initial PID namespace. On modern kernels with unprivileged BPF disabled, loading this program requires `CAP_BPF` plus `CAP_PERFMON`. `CAP_SYS_RESOURCE` and an appropriate `RLIMIT_MEMLOCK` are compatibility provisions for memlock handling; broader `CAP_SYS_ADMIN` is a compatibility fallback on older kernels, not a requirement of this guest run. The original host experiment temporarily granted `CAP_BPF`, `CAP_PERFMON`, and `CAP_SYS_RESOURCE`, then removed the file capabilities.

Production must define whether loading occurs in the daemon, a separate privileged loader/service, or packaging-time integration; how capabilities are bounded; how the object is installed and updated; and how loss causes reconciliation. Ad hoc file capabilities on a rebuilt benchmark binary are not a deployment design.

### BPF loader token and legal licensing: unresolved review

The kernel rejected the experimental object's original `SEC("license") = "LGPL"` token for GPL-restricted helpers. The accepted experimental loader declaration is `GPL and additional rights`, while the source carries `SPDX-License-Identifier: LGPL-3.0-only`.

Those statements serve different purposes. The fixed BPF license string is metadata consumed by the kernel verifier to decide helper availability. It does **not** itself grant recipients legal permission to copy, modify, or redistribute source or object code, and it does not settle compatibility between the object, loader, project, and linked/runtime components. Legal review of the loader/object distribution is required before production selection.

### Host NVML mismatch: measurement limitation, not guest failure

Host NVML could not initialize because the loaded host driver was 610.43.02 while host userland was 610.43.03. Consequently this continuation does not claim host NVML power or utilization measurements. Guest `nvidia-smi` validated the passed `[10de:2b85]` RTX 5090 with guest driver 610.43.03.

## Findings

### What worked

1. Final eBPF produced the lowest equal-duration `RUSAGE_SELF` CPU aggregate and the narrowest observed record stream **among the event backends**; its counting boundary is earlier-filtered than CN_PROC's. Polling observes zero event records by design.
2. CN_PROC delivered very low synthetic lifecycle latency and prompt live removal without an added capability grant in the tested initial user/PID namespace guest.
3. All event-backend live samples remained healthy.
4. All synthetic and live samples reported zero reconciliation requests.
5. Every synthetic timeout count was zero; the reported miss fields are aliases of those timeout outcomes, not independent observations.
6. Every live sample attributed one observed target PID to the controlled process group; each launcher survived its dwell, and termination was timed until that observed PID disappeared.
7. Periodic reconciliation remained available as loss recovery rather than the normal eBPF removal path.
8. VFIO/IOMMUFD passthrough, vCPU pinning, headless gamescope, and guest NVIDIA validation produced a usable displayless live-client environment.

### What remains

1. eBPF requires a supported capability/service, memlock, packaging, and upgrade design.
2. The BPF loader/object licensing needs legal review; the verifier token is not a legal grant.
3. CN_PROC remains noisy and namespace/kernel dependent.
4. Both event transports remain lossy, so bounded queues, health reporting, and reconciliation are required.
5. The measurements cover one guest/kernel/machine and do not establish system-wide power savings.
6. Live add latency is wrapper-dominated and cannot be compared directly with the older direct-launch host data.
7. Host NVML was unavailable, so GPU power/utilization was not measured from the host.
8. The live harness recorded start-time ticks but did not independently revalidate process generation at removal.
9. Event records carry TGID but no immutable generation identity, so rapid PID reuse can conflate events and tracked state from different generations without pidfd/start-time validation.
10. Exec/rename events for an already tracked process do not yet refresh immutable instance metadata.
11. The inspect-to-tracked-map handoff is not atomic with exit delivery and can transiently miss an exit. The final code now requests reconciliation for an eBPF exit that arrives untracked, closing the recovery gap, but that recovery signal cannot make inspection and publication atomic.
12. Tracked-map synchronization failure marks the backend unhealthy until restart, even if shadowed retries later converge.
13. The earlier nondeterministic unhealthy transition did not recur in ten invocations after bounded ring-poll `EINTR` retry, including nine authoritative final eBPF samples, but temporary diagnostics did not establish the original cause.

## Design reflection

The continuation changes the technical conclusion. The earlier report appropriately withheld eBPF because its exit event was effectively thrown away and ordinary removal fell through to five-second reconciliation. That userspace lifecycle bug, concurrent final-group EXIT amplification, and initial-map synchronization gap are corrected for the measured paths. The renamed-away preflight demonstrates the tested tracked-map path, while the remaining TGID identity, metadata-refresh, and inspect-to-track ordering blockers are stated above. In the equal-duration workload, final eBPF produced the narrowest observed record stream among the event backends and the lowest measured `RUSAGE_SELF` CPU aggregate with zero reconciliation requests.

The corrected design should preserve these invariants:

- production event identity must combine TGID with pidfd- or start-time-backed generation validation rather than trusting PID alone;
- an exit indication should be retained until matching-generation death is confirmed;
- a newer validated generation must supersede stale pending work;
- queue overflow, parse loss, or backend loss requests full reconciliation;
- periodic reconciliation remains correctness recovery, not ordinary event latency;
- filtering occurs as early as practical, but userspace validates `/proc` identity before publication; and
- initial/full-scan targets synchronize into a distinct tracked map with shadowed retry, while a bounded 128-key LRU tombstone suppresses duplicate final-group EXITs within its retained window.

CN_PROC remains valuable because it required no added capability in the tested initial user/PID namespace guest and had the fastest **live median** removal. Synthetic median removal was instead 85 µs for final eBPF and 106 µs for CN_PROC. CN_PROC should not be mistaken for “free”: its equal-duration run had more observed records, inspections, CPU, and voluntary context switches than either alternative. Further tracked-PID suppression could reduce repeated inspection, but the global connector stream and namespace restrictions remain architectural properties.

Polling remains necessary. It is portable, capability-free, and simple, and it supplies the reconciliation primitive required by both event transports. Its cost is predictable scan work and approximately one-second removal at the present lifecycle cadence.

The implementation choice should therefore be capability-driven rather than unconditional:

1. corrected eBPF on explicitly eligible and provisioned initial-PID-namespace Linux hosts after the remaining identity, metadata-refresh, and ordering blockers are resolved, retaining the exit/tombstone/map synchronization regressions;
2. CN_PROC where kernel/connector and initial-namespace eligibility checks pass and its event volume is acceptable;
3. polling everywhere else and as reconciliation for both event modes.

## Verification

The completed continuation verified:

- Arch guest kernel `7.1.4-zen1-1-zen` and QEMU `11.0.2`;
- 24 GiB guest memory and the 4-core/8-thread topology;
- exact vCPU0..7 pinning to `4,20,5,21,6,22,7,23` and all other QEMU threads to `8-15,24-31`;
- VFIO/IOMMUFD assignment of `[10de:2b85]` and guest `nvidia-smi` on driver `610.43.03`;
- the documented host NVML driver/userland mismatch;
- release builds of both benchmark examples with `linux-process-events`;
- one equal-duration synthetic JSON record per mode;
- 20 add, rename, and remove samples per synthetic mode;
- a five-second idle and 60-second/50 Hz/3,000-process churn lifecycle per synthetic mode;
- zero synthetic timeout outcomes (with miss fields aliasing those outcomes) and zero synthetic reconciliation requests;
- corrected renamed-away initial-target eBPF removal in 11.919 ms;
- one post-mitigation full synthetic eBPF attempt that became unhealthy and refused output, followed by the immediate identical successful authoritative rerun;
- three live JSON records per mode through headless gamescope;
- controlled process-group attribution and launcher survival in all nine live records;
- healthy CN_PROC and eBPF backends in all six applicable live records;
- zero live reconciliation requests;
- process-group termination/reap followed by disappearance of the observed target PID;
- the twelve named JSON files as the source of the synthetic and live benchmark-result tables (environment and pinning facts came from the separately observed guest/host configuration);
- the deterministic autoresearch harness over all 30 configured invocations, including discarded and failed hypotheses;
- exclusion of run 20 after its logged secondary payload was found not to match its captured benchmark output;
- a 497,204 us baseline combined metric and final unchanged-code combined metrics of 469,063, 472,509, and 470,135 us;
- nine final samples per event backend at exact 50 Hz/500-process churn, with healthy backends and zero timeout or reconciliation outcomes;
- final medians of 249.272 ms CN_PROC versus 220.863 ms eBPF total workload CPU, and 95.416 ms versus 62.643 ms self CPU; and
- direct verification that a reusable CN_PROC receive buffer reduced self CPU consistently, while four-datagram batching, fork-generation bypass, and candidate-state pre-lookup did not improve their measured paths.

The authoritative benchmark inputs are `/home/user/VMs/vosu-gaming/synthetic-poll.json`, `synthetic-cn-proc.json`, `synthetic-ebpf-final.json`, `live-poll-{1,2,3}.json`, `live-cn-proc-{1,2,3}.json`, and `live-ebpf-final-{1,2,3}.json`. They source the benchmark-result tables, not the environment or pinning tables. The earlier eBPF files and host measurements remain historical context only and do not replace these final results.

## Recommendation

**Do not select process events as the unconditional production default yet.**

Corrected eBPF is the strongest measured and technically preferred transport when a host passes explicit kernel, initial-PID-namespace, privilege, service, and licensing eligibility gates. In the final deterministic comparison it used 11.4% less total workload CPU and 34.3% less benchmark/runtime self CPU than CN_PROC, with 6.8% faster add and 26.7% faster removal. CN_PROC remained 17.6% faster for `comm` rename. eBPF is **not production-ready** until TGID generation identity, tracked-process metadata refresh, inspect-to-track exit ordering, conservative map-sync health behavior, and the remaining deployment and kernel-matrix gates are resolved and validated. CN_PROC is a no-added-capability but noisy alternative only where the connector and initial-namespace eligibility gates pass. Polling remains the portable fallback and the only option where event eligibility fails.

Before enabling eBPF in production:

1. define and review the capability/service and memlock deployment model;
2. define object installation, update, rollback, health, and loss/reconciliation behavior;
3. obtain legal review of the BPF source/object/loader licensing and distribution, without treating the kernel loader token as a license grant;
4. retain regression coverage for concurrent final-group exits within and beyond tombstone retention, renamed-away initial targets, generation-state clearing, and partial-success tracked-map retries;
5. add pidfd- or start-time-backed generation validation, refresh tracked immutable metadata on exec/rename, close the inspect-to-track exit recovery gap, preserve conservative unhealthy reporting on map-sync failure, and retain diagnostics/regression coverage for ring-poll failures even though bounded `EINTR` retry removed one known false-unhealthy path; and
6. validate all behavior on the supported kernel/distribution and initial-PID-namespace matrix.

Subject to those gates, the technical ordering is **eBPF -> CN_PROC -> polling**, selected by host capability rather than imposed as one universal default.
