---
type: Architecture Audit
title: Performance and memory audit
description: Evidence-based performance and memory opportunities, invariants, validation experiments, and designs to preserve.
tags: [architecture, performance, memory, profiling]
---

# Performance and memory audit


## Audit implementation status — 2026-09-05

This page retains earlier findings and measurement requirements below; it is not a list of entirely outstanding work. The audit implementation removed the report-only graphics dependency, isolated packaged-release smoke compilation, consolidated JSON numeric formatting, borrowed primary V1/V2/precise graph and hit-error arrays during encoding, reused prepared live-map work and retained replay batches, bounded counter discovery and report capture, bounded report log content/candidate retention, and moved console/configuration blocking work off async workers. WebSocket payload conversion now validates shared bytes without copying, and full pong queues close rather than stalling cancellation.

Linux process-event backends, default features, capability selection, recovery, and polling fallback are deliberately unchanged. Documentation is maintained Markdown with cheap local-link CI checks and no recurring inference. See [benchmarks](../testing/benchmarks.md) for measured results and limitations; remaining older recommendations below require separate evidence before implementation.

## Security implementation status — 2026-10-02

The earlier queue/retirement findings below are historical: relays now use a 16-event mailbox with nonblocking recipient delivery, compatibility control queues are bounded at 16 while state queues remain capacity one, and counter root generations share admission/cancellation/join accounting without a retired registry. This intentionally permits ordinary command bursts while bounding stalled recipients. Calculator input/rate/graph and decoded-collection budgets reject excessive work; no throughput improvement or process-wide aggregate memory bound is claimed. See the [security hardening notes](../operations/runbook.md#security-hardening-and-remaining-trust) for remaining risks.

## Executive summary

This audit tracks eight opportunities worth measuring. They are **candidates, not promises**: except where explicitly described as a local observation, their impact is inferred from static control-flow, allocation, ownership, and queue analysis. Linux idle discovery's broad metadata scan has since been addressed with name-first filtering. The highest-priority remaining candidates are repeated live-calculator work during map load, repeated manager reconstruction after coalesced state publication, and filtered WebSocket projection per client.

The safest sequence is to repair measurement first, instrument the production boundaries, and then change only paths shown to matter. Any optimization must preserve public API compatibility, one-point immutable atomic snapshots, generation/staleness checks, the existing bounded/latest-only capture, supervisor handoff, and client-delivery stages, the separation between live and legacy calculators, filesystem and authorization checks, and Linux/Windows semantics.

## Scope, evidence, and limitations

The review covered process discovery and native reads; regular and precise state capture; state reduction and publication; live and legacy calculation; API projection and WebSocket fan-out; configuration, counters, logging, reports, catalog downloads, and support services. The architectural boundaries and invariants are described in [architecture.md](../architecture/overview.md).

Evidence used:

- Static review of the source paths and symbols linked below.
- The existing Criterion scope and the machine-local observations recorded in [benchmarks.md](../testing/benchmarks.md). Values are intentionally not copied here: that document records the host, commands, confidence intervals, fixture limitations, and non-portability caveat.
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings -D clippy::perf` passed on 2026-07-16. This is static-lint evidence only; Clippy does not profile production runtime, syscall cost, allocation volume, queue growth, or peak RSS.

Important limitations:

- The Criterion memory fixtures do not perform native cross-process reads, and the five-object map is not representative of large or varied maps.
- Cold serializer benchmarks exclude snapshot construction; warm serializer benchmarks intentionally exercise cached `Bytes` clones.
- The publication benchmark is currently blocked: [`PublicationProvider::poll_rate`](../../benches/hot_paths.rs) returns 60 seconds while each iteration waits for delivery. The recorded 6000.2-second observation in [benchmarks.md](../testing/benchmarks.md) produced no usable publication result. This is a **measurement blocker**, not evidence of a production runtime regression. Fix the synthetic provider or benchmark cadence, not production throttling behavior.
- No heap profile, native-read trace, realistic multi-instance capture, filtered-client publication run, or concurrent calculation load test was available. All impact statements below are therefore labeled **Hypothesis** unless they describe source behavior directly.

## Prioritized findings

| Priority | Candidate | Static evidence | Primary validation |
| --- | --- | --- | --- |
| Addressed | Linux two-stage discovery while idle | `/proc/<pid>/comm` is filtered before command line, executable, bitness, and root reads | Native detection-latency and syscall profile if revisited |
| P0 | Repeated live-calculator work per map load | Difficulty/performance work is recomputed across initial, strains, and accuracy-table paths | Invocation counters, large-map CPU/allocation benchmark, output parity |
| P1 | Manager snapshot rebuild after coalesced state publication | Per-PID latest-only slots bound retained state; supervisor still rebuilds ordered manager metadata after each drained batch | Injected supervisor delay and multi-instance allocation/RSS profile |
| P1 | Precise hit-error history and mod-context cloning | Every precise publication copies cumulative hit errors; the precise loop separately clones variable-size mod context | Allocation profile versus hit-error/mod cardinality and cadence |
| P1 | Native remote-read batching | Linux reparses maps and allocates chunks/iovecs per `read_many`; capture readers issue multiple logical reads | Syscall/allocation trace against live stable and lazer processes |
| P1 | Filtered WebSocket projection duplication | Every filtered client independently builds and serializes projection trees | 1/10/100-client CPU, allocations, RSS, and byte-parity run |
| P1 | Bounded admission for legacy calculation | Synchronous parse/I/O/calculation executes from async requests without admission bound | Concurrent request p50/p99, Tokio utilization, queue/RSS profile |
| P1 | Whole-log and catalog response memory spikes | Reports whole-read retained logs; catalog may buffer and parse a response up to 1 GiB | Adversarial-size peak-RSS and cancellation tests |

## Detailed findings

### 1. Linux idle discovery filters before expensive metadata reads

**Classification:** Addressed; further event-driven work remains unproven.

**Source and mechanism.** [`InstanceRuntime`](../../src/instance.rs) still uses an `EMPTY_DISCOVERY_INTERVAL` of 100 ms so detection latency does not depend on privileged kernel event interfaces. On Linux, [`NativeProcessProvider::enumerate`](../../src/instance/discovery.rs) passes the stable/Wine and lazer name superset to [`enumerate_processes_matching`](../../src/process/linux.rs). Enumeration reads `comm` for each numeric `/proc` entry, then reads command line, executable link, ELF bitness, and the osu! root only after the name filter accepts the process. `classify_candidates` retains its named-first/fallback semantics.

**Current event-watcher decision (2026-09-05).** Retain the default Linux process-event feature for its measured performance benefit; see the dated [experiment evidence](../linux-process-events-experiment.md). Backend selection is capability-aware eBPF → CN_PROC → polling, not unconditional use of a privileged interface. Preserve bounded loss handling, PID identity checks, namespace/kernel fallback, and periodic reconciliation because neither event transport provides replay. Earlier advice against shipping the feature is superseded; historical measurements and their limitations remain unchanged.

**Invariant/risk.** Preserve Linux stable/Wine and lazer discovery, deterministic PID/generation handling, prompt empty-state polling, and the distinct Windows focus cadence. Do not cache process identity or resolved paths across PID reuse without the current generation checks.

**Future validation.** Before changing the polling interval or adding a wake mechanism, trace `/proc` files opened, discovery-loop wall/CPU time, and time-to-detect across realistic background-process counts and live stable/lazer starts. Any event path must prove loss recovery and namespace behavior while preserving the unprivileged polling path.

### 2. Live map load repeats calculator work

**Classification:** P0, **Hypothesis** (unmeasured map-load CPU and peak memory).

**Source and mechanism.** [`LiveBeatmapEngine::calculate`](../../src/calculation/state_backend.rs) computes difficulty, score/performance, strains, and eleven PP-by-accuracy points. In [`vosu-pp`'s scoring path](../../crates/vosu-pp/src/scoring.rs), `create_score` and performance calculation perform further difficulty/current-context work, so a single map load can repeat full-calculation stages. The map-switch path later creates progressive state from source text, while [`ParsedBeatmap`](../../crates/vosu-pp/src/beatmap.rs) owns original and playable forms; this may repeat parse/convert/mod application and raise transient RSS.

**Safe direction.** Add internal measured paths that reuse already-computed difficulty attributes, current context, score totals, or a prepared live map across strains, accuracy points, and progressive construction. Keep ownership bounded to the existing current-map cache and prefer `Arc` handoff over a second deep map copy.

**Invariant/risk.** Live state calculation and [`LegacyRosuEngine`](../../src/calculation/engine.rs) are separate compatibility contracts and must remain separate. Preserve exact calculator revisions, ruleset/mod/scoring/clock-rate cache dimensions, source identity invalidation, and result bytes/numerics.

**Validation experiment.** Count difficulty, conversion, strain, and performance invocations for map load. Benchmark representative large maps for every supported ruleset and representative mods, recording CPU, allocation bytes, and peak RSS. Require bit-for-bit result equality and live-parity checks before and after reuse.

### 3. State publications are coalesced; manager metadata is still rebuilt

**Classification:** P1, **Partially resolved** (bounded retention is covered; remaining rebuild cost is unmeasured).

**Current mechanism.** [`StatePublicationQueue`](../../src/runtime/worker.rs) retains at most one pending immutable snapshot per PID. A publication from the same generation replaces the pending value, while a late lower generation cannot replace a newer one. [`run_state_supervisor`](../../src/runtime/worker.rs) drains pending PIDs in deterministic order, rechecks each PID/generation against the desired lifecycle state, and emits one atomic manager snapshot for the batch. Focused regressions cover same-generation replacement, independent PIDs, deterministic ordering, and higher-generation precedence.

The supervisor still reconstructs `ready` and `desired` maps on each loop; `publish_manager_snapshot` creates another ready vector, semantic map, and instance values; [`ManagerSnapshot::new`](../../src/api/snapshot.rs) collects another ordered map. The latest-only handoff removes queue-age and unbounded-retention latency, but this reconstruction may still allocate more than necessary under high publication rates.

**Safe direction.** Retain lifecycle-derived ordered metadata between lifecycle/config changes and update only state `Arc`/revision fields where measurements show that reconstruction matters.

**Invariant/risk.** Preserve stale-publication rejection, cancellation/join behavior, deterministic PID order, focus/role semantics, semantic revisions, and one atomic all-instance `ManagerSnapshot`. A newer generation must never be replaced by an older publication.

**Validation experiment.** Instrument coalesced count, supervisor-loop time, allocations, and RSS. Run 1/8/32 workers with injected supervisor delays and mixed generation replacement. Verify bounded pending state, latest accepted publication, no stale generation reappearance, and unchanged atomic snapshot tests/live parity.

### 4. Precise publication copies cumulative history and mod context

**Classification:** P1, **Hypothesis** (unmeasured allocation/copy volume).

**Source and mechanism.** [`StateBundle::build_precise_state`](../../src/state/bundle/reducer.rs) creates a new `Arc` around a clone of `gameplay.hit_errors` and `key_overlay`; `publish_precise_state` invokes it on every accepted precise publication, so unchanged cumulative hit-error history is recopied at the precise cadence. Independently, the precise loop in [`spawn_state_worker_with_clock`](../../src/state/worker.rs) clones [`ReadContext`](../../src/state/context.rs), whose variable-size field is `CalculateMods`; hit-error history is not part of that context.

**Safe direction.** Cache and share immutable hit-error/key-overlay `Arc` values, replacing them only when their source vectors change. Separately, split the small precise synchronization fields from immutable mod context or share the latter with `Arc`. Avoid weakening suffix and mode/mod validation merely to reduce copies.

**Invariant/risk.** Precise samples must still be rejected when source, generation, sequence, mode/mod context, or hit-error suffix is stale. Latest-only capture and immutable snapshots are non-negotiable; sharing must not permit mutation behind a published value.

**Validation experiment.** Measure the two mechanisms separately: allocation stacks and copied bytes for no-new-hit versus appended-hit publications across 10/100/1,000 cumulative errors, then context-clone cost across small/large structured mod sets and configured precise cadences. Use adversarial reordered regular/precise samples to prove identical accept/reject behavior and byte-identical publications.

### 5. Consolidate native remote reads only where traces show repeated setup

**Classification:** P1, **Hypothesis** (native backend is absent from current benchmarks).

**Source and mechanism.** [`LinuxProcessMemory::read_many_inner`](../../src/process/linux.rs) calls `mapped_regions`, creates chunk and iovec vectors, and invokes `process_vm_readv`; short successful reads retry the remaining chunk suffix, while an error propagates and `read_many` zero-fills every destination. Repeated `read_many` calls in one logical capture therefore repeat `/proc/<pid>/maps` parsing and temporary construction. The regular and precise orchestration in [`SourceRegularReader`](../../src/state/readers.rs), [`SourcePreciseReader`](../../src/state/readers.rs), [`StableMemory`](../../src/memory/stable/readers.rs), and [`LazerMemory`](../../src/memory/lazer/reader.rs) determines whether independent field reads can be safely grouped. Windows has different staging/copy/retry behavior in [`src/process/windows.rs`](../../src/process/windows.rs).

**Safe direction.** Trace first. Where one capture issues multiple independent native batches, construct one bounded capture plan and reuse scratch capacity or one maps snapshot within that operation. Keep decoding above the OS backend and fall back with the same all-or-error semantics. Do not introduce a long-lived maps cache without an invalidation design.

**Invariant/risk.** Remote memory races are normal. Preserve exact destination fill, address/length validation, per-read error semantics, permissions, retry behavior, and platform-specific implementations. Larger batches must respect native iovec limits and must not turn one bad range into silent corruption elsewhere.

**Validation experiment.** Against live stable and lazer processes, record `process_vm_readv` calls, maps reads, iovec/chunk allocations, bytes requested/read, partial/error rates, capture latency, and CPU. Exercise mapping churn and target exit mid-read. Compare decoded snapshots and errors with the current path; run a separate Windows trace rather than extrapolating Linux results.

### 6. Filtered WebSocket clients duplicate projection work

**Classification:** P1, **Hypothesis** (filtered production path is not benchmarked).

**Source and mechanism.** [`ProductionSnapshotEncoder::encode`](../../src/runtime/projection.rs) builds a schema value, serializes through both `serde_json` and `serde_value`, materializes `ProjectionValue`, clones selected subtrees, and canonicalizes keys. [`WebSocketHub::run`](../../src/server/websocket.rs) calls filtered encoding independently for each filtered client. Identical filters therefore repeat full DTO/projection work; unlike unfiltered publication, they do not share the snapshot's cached `Bytes`.

**Safe direction.** Consider a streaming/lockstep projector or a **bounded per-generation** cache keyed by canonical filter only after profiles establish benefit. A cache must cap variant count and retained payload bytes; identical-filter sharing is preferable to a global unbounded filter cache.

**Invariant/risk.** Preserve JavaScript property/index ordering, requested filter ordering and nesting, numeric/non-finite behavior, exact JSON, per-client generation acknowledgements, one-slot data/control channels, and poll-rate throttling.

**Validation experiment.** Measure CPU, allocations, peak temporary and retained RSS, and output size for 1/10/100 clients with identical and unique filters across all four schemas, including nested objects, arrays, UTF-16-like index keys, and non-finite values. Require byte-for-byte output equality and verify cache bounds under continuously unique filters.

### 7. Legacy calculation needs bounded blocking admission

**Classification:** P1, **Hypothesis** (unmeasured concurrency amplification).

**Source and mechanism.** [`LegacyCalculateProvider::calculate`](../../src/server/service/providers.rs) invokes synchronous [`LegacyRosuEngine::calculate`](../../src/calculation/engine.rs) from an async request path. The current-map branch also reaches [`RuntimeBeatmap::current_beatmap_bytes`](../../src/runtime/service.rs), which uses synchronous `read_to_end`. Concurrent requests independently perform I/O, parsing/conversion, and calculation and can occupy Tokio workers or create excessive blocking work.

**Safe direction.** Put the synchronous stage behind `spawn_blocking` **and a bounded admission semaphore/queue**; make current-map I/O async or include it in the admitted blocking stage. Define overload behavior explicitly rather than spawning an unbounded blocking job per request.

**Invariant/risk.** Preserve the legacy setter order and outputs, current-map semantics, canonical `.osu` resolution, safe resource containment, authorization/body limits, cancellation/shutdown behavior, and separation from the live calculator. `spawn_blocking` alone is not a memory bound.

**Validation experiment.** Load test identical and different maps at increasing concurrency. Record admitted/queued/rejected counts, p50/p99 latency, Tokio worker utilization, blocking-pool concurrency, CPU, allocations, and RSS. Verify event-loop responsiveness and exact sequential-versus-concurrent result equality, including malformed and unauthorized requests.

### 8. Bound whole-log and catalog response memory

**Classification:** P1, **Hypothesis** (source-established whole-read behavior and catalog bound; unmeasured peak RSS).

**Source and mechanism.** [`read_newest_logs`](../../src/report.rs) reads up to ten retained logs with `read_to_string`, then parses/owns line strings before [`ReportBodyStream`](../../src/report.rs) renders them. [`logging::retention`](../../src/logging/retention.rs) begins cleanup when total logs reach 100 MiB but never deletes `latest.log`, so that threshold is not a hard bound; raw and parsed representations can overlap, and an active latest log can exceed the threshold. Separately, [`StreamingClient::request_inner`](../../src/net/streaming.rs) accumulates non-file responses in memory; [`CatalogService`](../../src/server/catalog.rs) allows `CATALOG_MAX_BYTES` of 1 GiB and then deserializes while retaining the response body. A malicious or accidental near-limit response can therefore create a large transient spike.

**Safe direction.** For reports, parse and escape logs incrementally or impose a documented report-specific byte budget while preserving ordering and redaction. For catalogs, lower the endpoint-specific cap to a justified size and/or stream JSON decoding from a bounded reader without retaining both complete encoded and decoded forms.

**Invariant/risk.** Preserve report escaping/redaction, newest-log ordering, cancellation, catalog schema/redirect/TLS policy, and download limits. Do not weaken updater archive size, extraction, path, published-checksum, or atomic-install protections while generalizing streaming code.

**Validation experiment.** Generate retained logs near the configured limit and valid/malformed catalog responses at stepped sizes. Measure peak RSS, allocations, first-byte/total latency, cancellation cleanup, and concurrent-request amplification. Assert exact report escaping/order and catalog results; verify deterministic rejection immediately above every new bound.

## Measurement gaps and roadmap

1. **Repair the publication benchmark.** Give its synthetic provider a zero/small benchmark-only cadence or otherwise make iterations cadence-aware with a timeout. Preserve the production test that rapid state events do not bypass poll throttling.
2. **Add observability before optimization.** Counters should cover discovery stages, native batch/syscall setup, context clone bytes, live-calculator invocation stages, publication queue age/coalescing, filtered projection allocation, legacy admission, and response bytes/peak concurrency.
3. **Build realistic local workloads.** Add developer-only scenarios for live stable/lazer native reads, large maps across rulesets/mods, multi-instance precise publication, filtered WebSocket clients, concurrent legacy requests, and near-limit logs/catalogs. Keep the pinned toolchain and identical Criterion/profile controls for paired runs.
4. **Use the right tools.** Criterion is suitable for isolated CPU/allocation comparisons once fixtures represent the path. Use a sampling profiler for daemon CPU and a heap/RSS profiler for retained/transient memory. Record host, revision, configuration, fixtures, sample controls, and confidence intervals in [benchmarks.md](../testing/benchmarks.md).
5. **Gate changes on behavior.** Performance comparisons must be paired with API byte parity, live parity where applicable, generation/staleness tests, queue-bound tests, platform-specific process tests, and security boundary tests. A faster result that changes semantics is a regression.

Lower-priority, cold/control-plane candidates should stay behind the roadmap above: [`RuntimeConfig::save`](../../src/runtime/config.rs) clones a JSON object on settings writes; [`RelayWorker`](../../src/server/service/relay.rs) has an unbounded event input and serial bounded recipient sends; [`ConfigStore`](../../src/config/store.rs) uses unbounded `StoreCommand` and filesystem-event queues; [`ReloadableCounterService::reload_static_root`](../../src/counters/service.rs) retains retired generations until shutdown; and [`RuntimePages::render`](../../src/runtime/service.rs) can repeat recursive counter discovery per page/search request. Validate each with burst/reload/request profiles and explicit queue/retention metrics before changing it. Preserve configuration publish barriers, relay targeting/replies, counter path validation, and static-root lifecycle semantics.

## Reviewed designs that should not be changed without evidence

- **Immutable atomic snapshots and encoding caches.** [`SnapshotManager`](../../src/api/snapshot.rs) publishes coherent `Arc` snapshots, and unfiltered HTTP/WebSocket consumers share cached `Bytes`. A `Bytes` clone is refcount work, not a payload copy. Do not replace this with mutable independently loaded schema pieces.
- **Bounded worker and client flow.** [`spawn_state_worker_with_clock`](../../src/state/worker.rs) uses one-slot regular backpressure and a latest-only precise slot; [`WebSocketHub`](../../src/server/websocket.rs) uses capacity-one data/control channels and generation acknowledgements. Keep these bounds even if adjacent unbounded queues are corrected.
- **Calculator separation and cache identity.** [`LiveStateCalculationBackend`](../../src/calculation/state_backend.rs) is already offloaded and serialized; parsed maps/graphs are shared with `Arc`, and [`BeatmapCache`](../../src/calculation/beatmap/cache.rs) validates source identity, checksum, mode, mods, scoring, and rate. Do not merge it with the legacy endpoint or weaken invalidation to obtain a cache hit.
- **Native read and platform boundaries.** [`ProcessMemory`](../../src/process.rs) requires complete-or-error reads; Linux and Windows backends intentionally differ. Keep pointer/length checks, generation validation, and decoding above native syscalls.
- **Security and reliability checks.** Authorization and request limits, [`resolve_resource`](../../src/server/files.rs) containment/secure-open behavior, catalog redirect/TLS limits, and updater archive/transaction checks are not optimization targets.
- **Streaming file responses and updater extraction.** [`file_response`](../../src/server/service/responses.rs) uses bounded range planning and streaming; [`extract_zip_atomic`](../../src/net/streaming/archive.rs) enforces limits and atomic replacement. No evidence supports replacing them.
- **Cold startup/report/config convenience clones.** CLI argument, report config-field, and small settings transformations are cold paths. They should not displace measured work on discovery, calculation, publication, projection, or bounded response memory.
