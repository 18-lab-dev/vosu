from __future__ import annotations

import bisect
import base64
import argparse
import hashlib
import json
import os
import math
import queue
from pathlib import Path
import shutil
import secrets
import socket
import signal
import struct
import subprocess
import sys
import tempfile
import time
import threading
import http.client
import urllib.error
import urllib.request
import tomllib
from typing import Any
from urllib.parse import urlsplit

sys.path.insert(0, str(Path(__file__).resolve().parent))
from live_parity import (SCHEMAS, PINNED_VOSU_VERSION, PINNED_SOURCE_REVISION, VOSU_BUILD_LABEL,
    SOURCE_BUILD_LABEL, SUPPORTED_MATRIX, Sample, align_active, barrier_of,
    beatmap_time, compare_pair, dump_capture, load_capture, monotonic_violations,
    paused_of, state_of)
from parity_domains import (
    cadence_evidence, confirmed_retry_boundaries, gamebase_path_family,
    infer_reference_capabilities, segment_at,
)
import parity_capture as _parity_capture
globals().update({name: value for name, value in vars(_parity_capture).items() if not name.startswith("__")})
import parity_verify as _parity_verify
globals().update({name: value for name, value in vars(_parity_verify).items() if not name.startswith("__")})
def _align_timestamp(
        source: list[Sample], vosu: list[Sample], window_ns: int,
) -> tuple[list[tuple[Sample, Sample]], list[Sample]]:
    """Maximize ordered timestamp matches within the comparison window."""
    ordered_vosu = sorted(vosu, key=lambda sample: sample.monotonic_ns)
    pairs: list[tuple[Sample, Sample]] = []
    unaligned: list[Sample] = []
    vosu_index = 0
    for left in sorted(source, key=lambda sample: sample.monotonic_ns):
        while (
                vosu_index < len(ordered_vosu)
                and ordered_vosu[vosu_index].monotonic_ns
                    < left.monotonic_ns - window_ns):
            vosu_index += 1
        if (
                vosu_index >= len(ordered_vosu)
                or ordered_vosu[vosu_index].monotonic_ns
                    > left.monotonic_ns + window_ns):
            unaligned.append(left)
            continue
        pairs.append((left, ordered_vosu[vosu_index]))
        vosu_index += 1
    return pairs, unaligned


def compare(capture_path: str, report_path: str) -> int:
    metadata, samples = load_capture(capture_path)
    verify_capture_artifacts(metadata, samples)
    poll = metadata.get("poll_rate_ms")
    if poll != 150:
        raise ValueError("capture poll_rate_ms is not the fixed release-gate value 150")
    if metadata.get("capture_interval_ms") != 25:
        raise ValueError(
            "capture interval_ms is not the fixed release-gate value 25")
    window_ms = max(2 * poll, 300)
    window_ns = window_ms * 1_000_000
    mismatches: list[Any] = []
    vosu_contract_failures: list[dict[str, Any]] = []
    reference_coverage_failures: list[dict[str, Any]] = []
    unobservable_coverage: list[dict[str, Any]] = []
    metrics: dict[str, Any] = {"dense_vosu_frames": {}, "http_probe_gaps": {}, "websocket_cadence": {}}
    missing: dict[str, list[str]] = {}
    missing_streams: list[str] = []
    v2_by_daemon = {
        daemon: sorted(
            (sample for sample in samples
             if sample.daemon == daemon and sample.schema == "v2"
             and sample.transport == "http"),
            key=lambda sample: sample.monotonic_ns)
        for daemon in ("source", "vosu")
    }
    v2_timestamps = {
        daemon: [sample.monotonic_ns for sample in v2_by_daemon[daemon]]
        for daemon in ("source", "vosu")
    }

    def nearest_v2_context(daemon: str, timestamp: int) -> Sample | None:
        ordered = v2_by_daemon[daemon]
        if not ordered:
            return None
        index = bisect.bisect_left(v2_timestamps[daemon], timestamp)
        candidates = ordered[max(0, index - 1):min(len(ordered), index + 1)]
        return min(
            candidates,
            key=lambda sample: abs(sample.monotonic_ns - timestamp),
        )
    expected = ("menu", "play", "paused", "result", "process-exit")
    barrier_contexts = _ordered_barrier_contexts(
        v2_by_daemon, poll, metadata.get("barrier_times"))
    for daemon in ("source", "vosu"):
        missing[daemon] = [
            barrier for barrier in expected
            if barrier not in barrier_contexts[daemon]
        ]
    capability_samples = [
        sample for sample in barrier_contexts["source"].values()
        if sample.schema == "v2" and sample.transport == "http"
    ]
    reference_capabilities = infer_reference_capabilities(capability_samples)

    def unobservable_policy(schema: str, parts: tuple[str, ...]) -> str | None:
        if reference_capabilities["gamebase"]["status"] == "ready":
            return None
        return gamebase_path_family(schema, parts)

    def compare_observation(*args: Any, **kwargs: Any) -> list[Any]:
        return compare_pair(
            *args, **kwargs, unobservable=unobservable_policy,
            coverage=unobservable_coverage)


    streams = {
        (daemon, schema, transport): sorted(
            (sample for sample in samples
             if sample.daemon == daemon and sample.schema == schema
             and sample.transport == transport),
            key=lambda sample: sample.monotonic_ns)
        for daemon in ("source", "vosu") for schema in SCHEMAS
        for transport in ("http", "websocket")
    }
    play_contexts = [
        barrier_contexts[daemon].get("play")
        for daemon in ("source", "vosu")
    ]
    pause_contexts = [
        barrier_contexts[daemon].get("paused")
        for daemon in ("source", "vosu")
    ]
    comparison_context_ready = all(play_contexts) and all(pause_contexts)
    shared_play_ns = max(
        (context.monotonic_ns for context in play_contexts if context),
        default=0,
    )
    shared_pause_ns = min(
        (context.monotonic_ns for context in pause_contexts if context),
        default=0,
    )

    def context_is_active(sample: Sample) -> bool:
        inside_shared_window = bool(
            comparison_context_ready
            and shared_play_ns <= sample.monotonic_ns < shared_pause_ns)
        if not inside_shared_window:
            return False
        if (
                sample.status != 200
                or sample.error is not None
                or not isinstance(sample.payload, dict)):
            return True
        barrier = barrier_of(sample)
        if barrier is None:
            context = nearest_v2_context(sample.daemon, sample.monotonic_ns)
            barrier = barrier_of(context) if context is not None else None
        return barrier == "play"
    v2_retry_boundaries = confirmed_retry_boundaries(
        [sample for sample in v2_by_daemon["source"] if context_is_active(sample)],
        [sample for sample in v2_by_daemon["vosu"] if context_is_active(sample)],
        window_ns, lambda payload: (
            payload.get("beatmap", {}).get("time", {}).get("live")
            if isinstance(payload, dict) else None))
    stream_retry_boundaries: dict[tuple[str, str, str], list[int]] = {}

    def stream_time(payload: Any, schema: str) -> float | None:
        if schema in {"v1", "v2"}:
            return beatmap_time(payload)
        if schema == "sc" and isinstance(payload, dict):
            value = payload.get("time", payload.get("mapPosition"))
            if isinstance(value, (int, float)) and not isinstance(value, bool):
                return float(value)
        return None

    def comparison_segment(sample: Sample) -> int:
        boundaries = stream_retry_boundaries.get(
            (sample.daemon, sample.schema, sample.transport),
            v2_retry_boundaries[sample.daemon])
        return segment_at(sample.monotonic_ns, boundaries)

    used: dict[tuple[str, str, str, str], set[int]] = {}

    def barrier_candidates(
            daemon: str, schema: str, transport: str, timestamp: int,
            barrier: str) -> list[tuple[int, int, Sample]]:
        key = (daemon, schema, transport, "barrier")
        return [
            (abs(sample.monotonic_ns - timestamp), index, sample)
            for index, sample in enumerate(streams[(daemon, schema, transport)])
            if index not in used.setdefault(key, set())
            and abs(sample.monotonic_ns - timestamp) <= window_ns
            and sample.status == 200
            and sample.error is None
            and isinstance(sample.payload, dict)
            and (
                (context := nearest_v2_context(
                    daemon, sample.monotonic_ns)) is not None
                and barrier_of(context) == barrier)
        ]

    for schema in SCHEMAS:
        for transport in ("http", "websocket"):
            raw_active = {
                daemon: [
                    sample for sample in streams[(daemon, schema, transport)]
                    if context_is_active(sample)
                ]
                for daemon in ("source", "vosu")
            }
            for daemon, daemon_samples in raw_active.items():
                for index, sample in enumerate(daemon_samples):
                    if (sample.status != 200 or sample.error is not None
                            or not isinstance(sample.payload, dict)):
                        missing_streams.append(
                            f"{transport}/{schema}/active-invalid-{daemon}-{index}")
                        opposite = "vosu" if daemon == "source" else "source"
                        candidates = [
                            other for other in raw_active[opposite]
                            if abs(other.monotonic_ns - sample.monotonic_ns) <= window_ns
                        ]
                        if candidates:
                            other = min(
                                candidates,
                                key=lambda value: abs(
                                    value.monotonic_ns - sample.monotonic_ns))
                            mismatches.extend(compare_observation(sample if daemon == "source" else other,
                            other if daemon == "source" else sample,
                            window_ms, active_override=True))
            active = {
                daemon: [
                    sample for sample in daemon_samples
                    if sample.status == 200 and sample.error is None
                    and isinstance(sample.payload, dict)
                ]
                for daemon, daemon_samples in raw_active.items()
            }
            local_boundaries = confirmed_retry_boundaries(
                active["source"], active["vosu"], window_ns,
                lambda payload, current_schema=schema: stream_time(
                    payload, current_schema))
            if local_boundaries["source"] and local_boundaries["vosu"]:
                for daemon in ("source", "vosu"):
                    stream_retry_boundaries[
                        (daemon, schema, transport)] = local_boundaries[daemon]
            if schema in {"v1", "v2"}:
                pairs, unaligned_source = align_active(
                    active["source"], active["vosu"], poll,
                    comparison_segment, comparison_segment)
                for left, right in pairs:
                    mismatches.extend(compare_observation(left, right, window_ms, active_override=True))
                for left in unaligned_source:
                    missing_streams.append(
                        f"{transport}/{schema}/active-source-"
                        f"{active['source'].index(left)}")
                surplus = len(active["vosu"]) - len(pairs)
                if surplus > 0:
                    metrics["dense_vosu_frames"][f"{transport}/{schema}"] = surplus
            else:
                pairs, unaligned_source = _align_timestamp(
                    active["source"], active["vosu"], window_ns)
                for left, right in pairs:
                    mismatches.extend(compare_observation(
                        left, right, window_ms, active_override=True))
                for left in unaligned_source:
                    missing_streams.append(
                        f"{transport}/{schema}/active-source-"
                        f"{active['source'].index(left)}")
                surplus = len(active["vosu"]) - len(pairs)
                if surplus > 0:
                    metrics["dense_vosu_frames"][
                        f"{transport}/{schema}"] = surplus
            if len(active["source"]) < 2 or len(active["vosu"]) < 2:
                missing_streams.append(f"{transport}/{schema}/active-coverage")

    for barrier in ("menu", "paused", "result"):
        for schema in SCHEMAS:
            for transport in ("http", "websocket"):
                left_context = barrier_contexts["source"].get(barrier)
                right_context = barrier_contexts["vosu"].get(barrier)
                if not left_context or not right_context:
                    continue
                left_candidates = barrier_candidates(
                    "source", schema, transport,
                    left_context.monotonic_ns, barrier)
                right_candidates = barrier_candidates(
                    "vosu", schema, transport,
                    right_context.monotonic_ns, barrier)
                promoted_times = metadata.get("barrier_times", {})
                left_floor = promoted_times.get("source", {}).get(
                    barrier, left_context.monotonic_ns)
                right_floor = promoted_times.get("vosu", {}).get(
                    barrier, right_context.monotonic_ns)
                left_candidates = [
                    candidate for candidate in left_candidates
                    if (
                        (context := nearest_v2_context(
                            "source", candidate[2].monotonic_ns)) is not None
                        and context.monotonic_ns >= left_floor
                    )
                ]
                right_candidates = [
                    candidate for candidate in right_candidates
                    if (
                        (context := nearest_v2_context(
                            "vosu", candidate[2].monotonic_ns)) is not None
                        and context.monotonic_ns >= right_floor
                    )
                ]
                if not left_candidates or not right_candidates:
                    if not left_candidates:
                        missing_streams.append(
                            f"{transport}/{schema}/{barrier}/source")
                    if not right_candidates:
                        missing_streams.append(
                            f"{transport}/{schema}/{barrier}/vosu")
                    continue
                bounded_pairs = [
                    (
                        abs(left.monotonic_ns - right.monotonic_ns),
                        left_distance + right_distance,
                        left.monotonic_ns,
                        right.monotonic_ns,
                        left_index,
                        right_index,
                        left,
                        right,
                    )
                    for left_distance, left_index, left in left_candidates
                    for right_distance, right_index, right in right_candidates
                    if abs(left.monotonic_ns - right.monotonic_ns) <= window_ns
                ]
                if not bounded_pairs:
                    missing_streams.append(
                        f"{transport}/{schema}/{barrier}/cross-daemon-bound")
                    continue
                *_, left_index, right_index, left, right = min(bounded_pairs)
                used[("source", schema, transport, "barrier")].add(left_index)
                used[("vosu", schema, transport, "barrier")].add(right_index)
                mismatches.extend(compare_observation(
                    left, right, window_ms,
                    active_override=barrier == "result"))

    exit_cutoff = metadata["client_process_exit"]["observed_monotonic_ns"]
    expected_envelope = {"error": "osu is not ready/running"}
    for schema in SCHEMAS:
        exit_candidates: dict[str, list[Sample]] = {}
        for daemon in ("source", "vosu"):
            candidates = [
                sample for sample in streams[(daemon, schema, "http")]
                if sample.monotonic_ns > exit_cutoff and sample.status == 500
                and sample.error == "not_ready" and sample.payload == expected_envelope
            ]
            exit_candidates[daemon] = candidates
            if not candidates:
                missing_streams.append(f"http/{schema}/fresh-process-exit/{daemon}")
        bounded_exit_pairs = [
            (abs(left.monotonic_ns - right.monotonic_ns), left, right)
            for left in exit_candidates["source"]
            for right in exit_candidates["vosu"]
            if abs(left.monotonic_ns - right.monotonic_ns) <= window_ns
        ]
        if (exit_candidates["source"] and exit_candidates["vosu"]
                and not bounded_exit_pairs):
            missing_streams.append(
                f"http/{schema}/fresh-process-exit/cross-daemon-bound")
        if bounded_exit_pairs:
            _, left, right = min(
                bounded_exit_pairs,
                key=lambda value: (
                    value[0], value[1].monotonic_ns, value[2].monotonic_ns))
            mismatches.extend(compare_observation(left, right, window_ms, active_override=False))


    active_streams: dict[tuple[str, str, str], list[Sample]] = {}
    for key, stream in streams.items():
        active_streams[key] = [
            sample for sample in stream if context_is_active(sample)]
    monotonic_samples = [
        sample for stream in active_streams.values() for sample in stream
    ]
    mismatches.extend(monotonic_violations(
        monotonic_samples, context_is_active, comparison_segment))

    for schema in SCHEMAS:
        for transport in ("http", "websocket"):
            maximum_ms = 3 * poll
            for daemon in ("source", "vosu"):
                ordered = active_streams[(daemon, schema, transport)]
                points = [sample.monotonic_ns for sample in ordered]
                evidence = cadence_evidence(points, maximum_ms)
                key = f"{daemon}/{schema}"
                if transport == "http":
                    # HTTP timestamps are response-completion probe latency only.
                    metrics["http_probe_gaps"][key] = evidence
                    continue
                metrics["websocket_cadence"][key] = evidence
                if evidence["stall_count"]:
                    failure = {
                        "schema": schema,
                        "transport": "websocket",
                        "daemon": daemon,
                        "category": (
                            "reference_coverage" if daemon == "source"
                            else "vosu_contract"),
                        "reason": "absolute_ws_stall",
                        "stall_count": evidence["stall_count"],
                        "maximum_gap_ms": evidence["maximum_gap_ms"],
                        "threshold_ms": maximum_ms,
                    }
                    if daemon == "source":
                        reference_coverage_failures.append(failure)
                    else:
                        vosu_contract_failures.append(failure)

    for stream in sorted(set(missing_streams)):
        if "/vosu" in stream or "vosu-" in stream:
            vosu_contract_failures.append({
                "category": "vosu_contract",
                "reason": "missing_vosu_observation",
                "stream": stream,
            })
        else:
            reference_coverage_failures.append({
                "category": "reference_coverage",
                "reason": "unpaired_or_missing_source_observation",
                "stream": stream,
            })

    serial = [value if isinstance(value, dict) else value.__dict__
              for value in mismatches]
    unique_unobservable = sorted({
        (item["schema"], item["path_family"], item["reason"])
        for item in unobservable_coverage
    })
    coverage = {
        "reference_capabilities": reference_capabilities,
        "unobservable_reference": [
            {"schema": schema, "path_family": family, "reason": reason,
             "observations_excluded": sum(
                 item["schema"] == schema and item["path_family"] == family
                 for item in unobservable_coverage)}
            for schema, family, reason in unique_unobservable
        ],
        "reference_failures": reference_coverage_failures,
    }
    unresolved_required_domain = (
        reference_capabilities["gamebase"]["status"] != "ready")
    hard_failure = bool(
        serial or vosu_contract_failures or any(missing.values())
        or missing_streams)
    status = (
        "failed" if hard_failure
        else "inconclusive" if unresolved_required_domain
        or reference_coverage_failures
        else "passed")

    report = {
        "format": 2, "metadata": metadata,
        "process_ownership": metadata.get("process_ownership"),
        "client_process_exit": metadata.get("client_process_exit"),
        "status": status,
        "passed": status == "passed",
        "missing_barriers": missing,
        "missing_streams": sorted(set(missing_streams)),
        "mismatches": serial,
        "parity_mismatches": serial,
        "vosu_contract_failures": vosu_contract_failures,
        "coverage": coverage,
        "metrics": metrics,
        "summary": {
            "samples": len(samples),
            "mismatches": len(serial),
            "vosu_contract_failures": len(vosu_contract_failures),
            "reference_coverage_failures": len(reference_coverage_failures),
            "unobservable_path_families": len(unique_unobservable),
            "status": status,
        },
    }

    with open(report_path, "w", encoding="utf-8") as destination:
        json.dump(report, destination, indent=2, sort_keys=True, ensure_ascii=False)
        destination.write("\n")
    print(json.dumps(report["summary"], sort_keys=True))
    return 0 if report["passed"] else 1
