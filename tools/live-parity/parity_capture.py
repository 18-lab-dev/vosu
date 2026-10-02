from __future__ import annotations

import bisect
import base64
import argparse
import hashlib
import json
import os
import math
import gc
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
import parity_process as _parity_process
globals().update({name: value for name, value in vars(_parity_process).items() if not name.startswith("__")})
import parity_artifacts as _parity_artifacts
globals().update({name: value for name, value in vars(_parity_artifacts).items() if not name.startswith("__")})
import parity_probes as _parity_probes
globals().update({name: value for name, value in vars(_parity_probes).items() if not name.startswith("__")})
import parity_build as _parity_build
globals().update({name: value for name, value in vars(_parity_build).items() if not name.startswith("__")})
import parity_daemon as _parity_daemon
globals().update({name: value for name, value in vars(_parity_daemon).items() if not name.startswith("__")})
MIN_ACTIVE_BEFORE_PAUSE_NS = 1_000_000_000
MIN_SHARED_PLAY_STABLE_NS = 2_000_000_000
def _websocket_barrier_window_ns(poll_rate_ms: int) -> int:
    return max(4 * poll_rate_ms, 600) * 1_000_000
def _valid_v2_sample(sample: Sample | None, barrier: str) -> bool:
    return bool(
        sample is not None
        and sample.status == 200
        and sample.error is None
        and isinstance(sample.payload, dict)
        and barrier_of(sample) == barrier
    )


def _map_identity(sample: Sample) -> tuple[str, Any] | None:
    beatmap = sample.payload.get("beatmap")
    if not isinstance(beatmap, dict):
        return None
    for key in ("checksum", "id", "title"):
        value = beatmap.get(key)
        if isinstance(value, str) and value.strip():
            return key, value.strip().lower() if key == "checksum" else value.strip()
        if key == "id" and isinstance(value, int) and not isinstance(value, bool) and value > 0:
            return key, value
    return None


def _shared_map_identity(samples: list[Sample], allow_no_map: bool = False) -> bool:
    identities = [_map_identity(sample) for sample in samples]
    return (
        identities[0] is not None and identities[0] == identities[1]
    ) or (allow_no_map and identities == [None, None])


def _shared_menu_ready(
        current_samples: dict[str, Sample | None], poll_rate_ms: int) -> bool:
    samples = [current_samples.get(daemon) for daemon in ("source", "vosu")]
    if any(not _valid_v2_sample(sample, "menu") for sample in samples):
        return False
    valid = [sample for sample in samples if sample is not None]
    if abs(valid[0].monotonic_ns - valid[1].monotonic_ns) > max(2 * poll_rate_ms, 300) * 1_000_000:
        return False
    if not _shared_map_identity(valid, allow_no_map=True):
        return False
    for sample in valid:
        client = sample.payload.get("client")
        settings = sample.payload.get("settings")
        identity = settings.get("client") if isinstance(settings, dict) else None
        version = identity.get("version") if isinstance(identity, dict) else None
        # Upstream reports HTTP 200/menu while its initial memory read is pending.
        if (
                not isinstance(client, str) or not client.strip()
                or not isinstance(version, str) or not version.strip()):
            return False
        beatmap = sample.payload.get("beatmap")
        timing = beatmap.get("time") if isinstance(beatmap, dict) else None
        audio_length = timing.get("mp3Length") if isinstance(timing, dict) else None
        # A selected map can precede the source's first audio-length read.
        if _map_identity(sample) is not None and (
                not isinstance(audio_length, (int, float))
                or isinstance(audio_length, bool)
                or not math.isfinite(audio_length)
                or audio_length < 0):
            return False
    return True


def _shared_play_ready(
        current_samples: dict[str, Sample | None], poll_rate_ms: int) -> bool:
    samples = [current_samples.get(daemon) for daemon in ("source", "vosu")]
    if any(not _valid_v2_sample(sample, "play") for sample in samples):
        return False
    valid = [sample for sample in samples if sample is not None]
    if not _shared_map_identity(valid):
        return False
    times: list[float] = []
    first_objects: list[float] = []
    for sample in valid:
        play = sample.payload.get("play")
        player_name = play.get("playerName") if isinstance(play, dict) else None
        # Lazer can report play with the preview clock before its score is ready.
        if not isinstance(player_name, str) or not player_name.strip():
            return False
        beatmap = sample.payload.get("beatmap")
        timing = beatmap.get("time") if isinstance(beatmap, dict) else None
        live = timing.get("live") if isinstance(timing, dict) else None
        first_object = timing.get("firstObject") if isinstance(timing, dict) else None
        if any(
                not isinstance(value, (int, float))
                or isinstance(value, bool)
                or not math.isfinite(value)
                for value in (live, first_object)):
            return False
        times.append(float(live))
        first_objects.append(float(first_object))
    return (
        all(value >= first for value, first in zip(times, first_objects))
        and abs(times[0] - times[1]) <= max(2 * poll_rate_ms, 300)
    )


def _shared_play_stable(
        current_samples: dict[str, Sample | None],
        poll_rate_ms: int,
        ready_since_ns: int | None,
) -> tuple[bool, int | None]:
    if not _shared_play_ready(current_samples, poll_rate_ms):
        return False, None
    observed_ns = min(
        sample.monotonic_ns
        for sample in current_samples.values()
        if sample is not None
    )
    if ready_since_ns is None:
        return False, observed_ns
    return (
        observed_ns - ready_since_ns >= MIN_SHARED_PLAY_STABLE_NS,
        ready_since_ns,
    )


def _shared_result_ready(
        current_samples: dict[str, Sample | None]) -> bool:
    samples = [current_samples.get(daemon) for daemon in ("source", "vosu")]
    if any(not _valid_v2_sample(sample, "result") for sample in samples):
        return False
    valid = [sample for sample in samples if sample is not None]
    if not _shared_map_identity(valid):
        return False
    for sample in valid:
        result = sample.payload.get("resultsScreen")
        player_name = result.get("name") if isinstance(result, dict) else None
        if not isinstance(player_name, str) or not player_name.strip():
            return False
    return True


def _capture_barrier_ready(
        barrier: str | None, monotonic_ns: int,
        barrier_times: dict[str, int],
        current_barriers: dict[str, str | None]) -> bool:
    if barrier != "paused":
        return True
    play_ns = barrier_times.get("play")
    return (
        play_ns is not None
        and monotonic_ns - play_ns >= MIN_ACTIVE_BEFORE_PAUSE_NS
        and all(current_barriers.get(daemon) == "paused"
                for daemon in ("source", "vosu"))
    )


def _ordered_barrier_contexts(
        samples_by_daemon: dict[str, list[Sample]],
        poll_rate_ms: int,
        promoted_times: dict[str, dict[str, int]] | None = None,
) -> dict[str, dict[str, Sample]]:
    contexts: dict[str, dict[str, Sample]] = {"source": {}, "vosu": {}}
    current_barriers: dict[str, str | None] = {"source": None, "vosu": None}
    current_samples: dict[str, Sample | None] = {"source": None, "vosu": None}

    def reached_promoted_time(barrier: str) -> bool:
        if promoted_times is None:
            return True
        return all(
            current_samples[daemon] is not None
            and current_samples[daemon].monotonic_ns
                >= promoted_times.get(daemon, {}).get(barrier, 0)
            for daemon in ("source", "vosu")
        )
    expected = ("menu", "play", "paused", "result", "process-exit")
    synchronized_positions = {"menu": 0, "result": 3, "process-exit": 4}
    ordered = sorted(
        (sample for daemon_samples in samples_by_daemon.values()
         for sample in daemon_samples),
        key=lambda sample: sample.monotonic_ns)
    for sample in ordered:
        barrier = barrier_of(sample)
        current_barriers[sample.daemon] = barrier
        current_samples[sample.daemon] = sample
        if barrier in synchronized_positions:
            if (
                    reached_promoted_time(barrier)
                    and all(current_barriers[daemon] == barrier
                            for daemon in ("source", "vosu"))
                    and (
                        promoted_times is not None
                        or (barrier == "menu"
                            and _shared_menu_ready(current_samples, poll_rate_ms))
                        or (barrier == "result" and _shared_result_ready(current_samples))
                        or barrier == "process-exit"
                    )):
                for daemon in ("source", "vosu"):
                    if (
                            len(contexts[daemon]) == synchronized_positions[barrier]
                            and current_samples[daemon] is not None):
                        contexts[daemon][barrier] = current_samples[daemon]
            continue
        if (
                barrier == "play"
                and reached_promoted_time("play")
                and (
                    promoted_times is not None
                    or _shared_play_ready(current_samples, poll_rate_ms))):
            for daemon in ("source", "vosu"):
                if (
                        len(contexts[daemon]) == 1
                        and current_barriers[daemon] == "play"
                        and current_samples[daemon] is not None):
                    contexts[daemon]["play"] = current_samples[daemon]
            continue
        if barrier == "paused":
            for daemon in ("source", "vosu"):
                daemon_sample = current_samples[daemon]
                daemon_play = contexts[daemon].get("play")
                if (
                        reached_promoted_time("paused")
                        and len(contexts[daemon]) == 2
                        and daemon_sample is not None
                        and daemon_play is not None
                        and _capture_barrier_ready(
                            "paused",
                            daemon_sample.monotonic_ns,
                            {"play": daemon_play.monotonic_ns},
                            current_barriers,
                        )):
                    contexts[daemon]["paused"] = daemon_sample
            if all("paused" in contexts[daemon] for daemon in ("source", "vosu")):
                continue
        position = len(contexts[sample.daemon])
        barrier_times = {
            name: context.monotonic_ns
            for name, context in contexts[sample.daemon].items()
        }
        if (
                position < len(expected)
                and barrier == expected[position]
                and reached_promoted_time(barrier)
                and (barrier != "play"
                     or promoted_times is not None
                     or _shared_play_ready(current_samples, poll_rate_ms))
                and _capture_barrier_ready(
                    barrier, sample.monotonic_ns, barrier_times,
                    current_barriers)):
            contexts[sample.daemon][barrier] = sample
    return contexts

def _http_samples_attested(
        connection_checks: list[dict[str, Any]],
        http_timestamps: list[int]) -> bool:
    if not http_timestamps:
        return not connection_checks
    if len(connection_checks) < len(http_timestamps):
        return False
    previous_timestamp = -1
    for check, timestamp in zip(connection_checks, http_timestamps):
        if (check["before_monotonic_ns"] <= previous_timestamp
                or check["after_monotonic_ns"] > timestamp):
            return False
        previous_timestamp = timestamp
    return all(
        check["after_monotonic_ns"] > http_timestamps[-1]
        for check in connection_checks[len(http_timestamps):])



def capture(cfg: dict[str, Any], out: str, timeout: float, interval_ms: int) -> int:
    if isinstance(timeout, bool) or not isinstance(timeout, (int, float)) \
            or not math.isfinite(timeout) or timeout <= 0:
        raise ValueError("timeout must be a finite positive number")
    if interval_ms != 25:
        raise ValueError("capture interval_ms must be exactly 25")
    if not isinstance(timeout, (int, float)) or not 0 < timeout < float("inf"):
        raise ValueError("capture timeout must be finite and positive")
    build_attestation = build_release_artifacts(cfg, out)
    processes, ownership = launch(cfg)
    client = cfg["_artifact_identity"]["client"]
    client_process: subprocess.Popen[bytes] | None = None
    gc_was_enabled = gc.isenabled()
    samples: list[Sample] = []
    seen: dict[str, list[str]] = {"source": [], "vosu": []}
    barrier_times: dict[str, dict[str, int]] = {"source": {}, "vosu": {}}
    current_barriers: dict[str, str | None] = {"source": None, "vosu": None}
    current_samples: dict[str, Sample | None] = {"source": None, "vosu": None}
    play_ready_since_ns: int | None = None

    def promote_current_barrier(barrier: str) -> None:
        for daemon in ("source", "vosu"):
            sample = current_samples[daemon]
            assert sample is not None
            seen[daemon].append(barrier)
            barrier_times[daemon][barrier] = sample.monotonic_ns
            print(f"{daemon}: reached {barrier}", file=sys.stderr, flush=True)
    exit_schemas: dict[str, set[str]] = {"source": set(), "vosu": set()}
    client_exit_observed_ns: int | None = None
    ws_failures: list[str] = []
    http_failures: list[str] = []
    lock = threading.Lock()
    stop = threading.Event()
    try:
        # Retained JSON/sample trees are acyclic; cyclic GC pauses grow during capture.
        gc.disable()
        client_process, client_evidence = _launch_client(cfg)
        ws_threads = [
            threading.Thread(
                target=websocket_capture,
                args=(cfg["daemons"][daemon]["base_url"].rstrip("/"), daemon, schema,
                      stop, samples, lock, ownership[daemon], ws_failures),
                daemon=True, name=f"vosu-live-parity-{daemon}-{schema}")
            for daemon in ("source", "vosu") for schema in SCHEMAS
        ]

        http_results: queue.SimpleQueue[Sample] = queue.SimpleQueue()
        next_http_start = time.monotonic()

        def schedule_http_round() -> None:
            nonlocal next_http_start
            next_http_start = max(
                next_http_start + interval_ms / 1000, time.monotonic())

        # Independent probe loops drift when ownership checks have different costs.
        # Keep every real response, but start all eight probes in the same round.
        http_round = threading.Barrier(2 * len(SCHEMAS), action=schedule_http_round)
        http_probes = {
            (daemon, schema): HttpProbe(
                cfg["daemons"][daemon]["base_url"].rstrip("/"),
                daemon, schema, ownership[daemon])
            for daemon in ("source", "vosu") for schema in SCHEMAS
        }

        def capture_http(daemon: str, schema: str) -> None:
            with http_probes[daemon, schema] as probe:
                while not stop.is_set():
                    try:
                        http_round.wait()
                    except threading.BrokenBarrierError:
                        return
                    if stop.wait(max(0.0, next_http_start - time.monotonic())):
                        return
                    try:
                        http_results.put(probe.fetch())
                    except Exception as exc:
                        http_failures.append(
                            f"{daemon}/{schema}: {type(exc).__name__}: {exc}")
                        stop.set()
                        http_round.abort()
                        return

        http_threads = [
            threading.Thread(
                target=capture_http, args=(daemon, schema), daemon=True,
                name=f"vosu-live-parity-http-{daemon}-{schema}")
            for daemon in ("source", "vosu") for schema in SCHEMAS
        ]
        _assert_runtime_ownership(ownership, "capture-start")
        deadline = time.monotonic() + timeout
        try:
            for thread in [*ws_threads, *http_threads]:
                thread.start()
            while time.monotonic() < deadline:
                if ws_failures:
                    raise ValueError(
                        "websocket ownership verification failed: "
                        + "; ".join(ws_failures))
                if http_failures:
                    raise ValueError(
                        "HTTP ownership verification failed: "
                        + "; ".join(http_failures))

                batch: list[Sample] = []
                while True:
                    try:
                        batch.append(http_results.get_nowait())
                    except queue.Empty:
                        break
                if batch:
                    with lock:
                        samples.extend(batch)

                v2_batch = sorted(
                    (sample for sample in batch if sample.schema == "v2"),
                    key=lambda sample: sample.monotonic_ns,
                )
                for sample in v2_batch:
                    barrier = barrier_of(sample)
                    if barrier == "process-exit":
                        continue
                    current_barriers[sample.daemon] = barrier
                    current_samples[sample.daemon] = sample
                    if not all(
                            current_barriers[daemon] == "play"
                            for daemon in ("source", "vosu")):
                        play_ready_since_ns = None

                    synchronized_position = {
                        "menu": 0,
                        "result": 3,
                    }.get(barrier)
                    if synchronized_position is not None:
                        if (
                                all(current_barriers[daemon] == barrier
                                    for daemon in ("source", "vosu"))
                                and (
                                    (barrier == "menu"
                                     and _shared_menu_ready(current_samples, cfg["poll_rate_ms"]))
                                    or (barrier == "result"
                                        and _shared_result_ready(current_samples))
                                )
                                and all(len(seen[daemon]) == synchronized_position
                                        for daemon in ("source", "vosu"))):
                            promote_current_barrier(barrier)
                        continue

                    if barrier == "play":
                        play_stable, play_ready_since_ns = _shared_play_stable(
                            current_samples,
                            cfg["poll_rate_ms"],
                            play_ready_since_ns,
                        )
                        if (
                                all(len(seen[daemon]) == 1
                                    for daemon in ("source", "vosu"))
                                and play_stable):
                            promote_current_barrier("play")
                        continue

                    if barrier == "paused" and (
                            all(len(seen[daemon]) == 2
                                for daemon in ("source", "vosu"))
                            and all(_capture_barrier_ready(
                                "paused",
                                current_samples[daemon].monotonic_ns,
                                barrier_times[daemon],
                                current_barriers,
                            ) for daemon in ("source", "vosu"))):
                        promote_current_barrier("paused")

                if client_process is not None:
                    client_process.poll()
                results_ready = all(
                    "result" in reached for reached in seen.values())
                client_alive = _assert_client_population(client, results_ready)
                if client_exit_observed_ns is None and not client_alive:
                    client_exit_observed_ns = time.monotonic_ns()
                for sample in batch:
                    if (client_exit_observed_ns is not None
                            and sample.monotonic_ns > client_exit_observed_ns
                            and sample.status == 500 and sample.error == "not_ready"
                            and sample.payload == {"error": "osu is not ready/running"}):
                        exit_schemas[sample.daemon].add(sample.schema)
                for sample in batch:
                    if sample.schema != "v2" or barrier_of(sample) != "process-exit":
                        continue
                    if exit_schemas[sample.daemon] != set(SCHEMAS):
                        continue
                    if len(seen[sample.daemon]) == 4:
                        seen[sample.daemon].append("process-exit")
                        barrier_times[sample.daemon]["process-exit"] = sample.monotonic_ns
                        print(f"{sample.daemon}: reached process-exit",
                              file=sys.stderr, flush=True)
                if all(len(value) == 5 for value in seen.values()):
                    break
                stop.wait(interval_ms / 1000)
        finally:
            stop.set()
            http_round.abort()
            for thread in [*http_threads, *ws_threads]:
                if thread.ident is not None:
                    thread.join(timeout=3)
            remaining_http: list[Sample] = []
            while True:
                try:
                    remaining_http.append(http_results.get_nowait())
                except queue.Empty:
                    break
            if remaining_http:
                with lock:
                    samples.extend(remaining_http)
            if ws_failures:
                raise ValueError(
                    "websocket ownership verification failed: " + "; ".join(ws_failures))
            if http_failures:
                raise ValueError(
                    "HTTP ownership verification failed: " + "; ".join(http_failures))
            _assert_runtime_ownership(ownership, "before-shutdown")
        for record in ownership.values():
            record["ownership_checks"].sort(
                key=lambda check: (check["before_monotonic_ns"],
                                   check["after_monotonic_ns"]))
        captured_v2 = {
            daemon: sorted(
                (sample for sample in samples
                 if sample.daemon == daemon and sample.schema == "v2"
                 and sample.transport == "http"),
                key=lambda sample: sample.monotonic_ns)
            for daemon in ("source", "vosu")
        }
        captured_contexts = _ordered_barrier_contexts(
            captured_v2, cfg["poll_rate_ms"], barrier_times)
        reference_capabilities = infer_reference_capabilities(
            captured_contexts["source"].values())
        metadata = {
            "os": cfg["os"].lower(), "client": cfg["client"].lower(),
            "poll_rate_ms": cfg["poll_rate_ms"], "capture_interval_ms": interval_ms,
            "artifacts": cfg["_artifact_identity"],
            "process_ownership": ownership, "client_launch": client_evidence,
            "client_process_exit": {
                "pid": client["pid"], "start_identity": client["start_identity"],
                "approved_processes": client["approved_processes"],
                "observed_terminated": not any(
                    _client_record_alive(record)
                    for record in client["approved_processes"]),
                "observed_monotonic_ns": client_exit_observed_ns,
                "exact_not_ready_schemas": {
                    daemon: sorted(values) for daemon, values in exit_schemas.items()
                },
            },
            "build_attestation": build_attestation,
            "http_paths": SCHEMAS, "websocket_paths": WS_PATHS,
            "http_transport": {
                daemon: {
                    schema: {
                        "connection_attempts": http_probes[daemon, schema].connection_attempts,
                        "request_attempts": http_probes[daemon, schema].request_attempts,
                    }
                    for schema in SCHEMAS
                }
                for daemon in ("source", "vosu")
            },
            "reference_capabilities": reference_capabilities,
            "barrier_times": barrier_times,
        }
        if "scenario" in cfg:
            metadata["scenario"] = cfg["scenario"]
        dump_capture(out, samples, metadata)
    finally:
        if gc_was_enabled:
            gc.enable()
        _close_windows_client_job(client)
        _terminate_client_descendants(client)
        if client_process is not None:
            _signal_process_tree(client_process)
        for process in processes.values():
            _signal_process_tree(process)
        for process in [*(processes.values()), *([client_process] if client_process else [])]:
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                _signal_process_tree(process, hard=True)
        if not _wait_for_client_descendants(client, 5):
            _terminate_client_descendants(client, hard=True)
            if not _wait_for_client_descendants(client, 1):
                raise ValueError(
                    "attested osu client descendants survived cleanup")
        for process in processes.values():
            runtime_dir = getattr(process, "_live_parity_runtime_dir", None)
            if isinstance(runtime_dir, Path):
                shutil.rmtree(runtime_dir, ignore_errors=True)
            config_path = getattr(process, "_live_parity_seeded_config", None)
            if isinstance(config_path, Path):
                config_path.unlink(missing_ok=True)
    missing = {
        daemon: [barrier for barrier in ("menu", "play", "paused", "result", "process-exit")
                 if barrier not in reached]
        for daemon, reached in seen.items()
    }
    missing_streams = []
    window_ns = _websocket_barrier_window_ns(cfg["poll_rate_ms"])
    for daemon in ("source", "vosu"):
        for schema in SCHEMAS:
            stream = sorted(
                (sample for sample in samples
                 if sample.daemon == daemon and sample.schema == schema
                 and sample.transport == "websocket"),
                key=lambda sample: sample.monotonic_ns)
            for barrier in ("menu", "paused", "result"):
                target = barrier_times[daemon].get(barrier)
                if target is None or not any(
                        abs(sample.monotonic_ns - target) <= window_ns for sample in stream):
                    missing_streams.append(f"{daemon}/{schema}/{barrier}")
    if any(missing.values()) or missing_streams:
        print(json.dumps({
            "error": "capture_incomplete", "missing_barriers": missing,
            "missing_websocket_streams": missing_streams,
        }, sort_keys=True), file=sys.stderr)
        return 2
    return 0
