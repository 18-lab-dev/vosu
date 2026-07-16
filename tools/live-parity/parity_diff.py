"""Export synchronized full API payloads and human-readable unified diffs."""
from __future__ import annotations

import difflib
import json
from pathlib import Path
from typing import Any

from live_parity import SCHEMAS, Sample, load_capture
from parity_capture import _ordered_barrier_contexts, _websocket_barrier_window_ns

_BARRIERS = ("menu", "play", "paused", "result", "process-exit")
_TRANSPORTS = ("http", "websocket")


def _canonical(sample: Sample) -> str:
    envelope: dict[str, Any] = {
        "status": sample.status,
        "error": sample.error,
        "payload": sample.payload,
    }
    return json.dumps(envelope, indent=2, sort_keys=True, ensure_ascii=False) + "\n"


def _nearest(
    samples: list[Sample], daemon: str, schema: str, transport: str,
    target_ns: int, window_ns: int,
) -> Sample | None:
    candidates = [
        sample for sample in samples
        if sample.daemon == daemon
        and sample.schema == schema
        and sample.transport == transport
        and abs(sample.monotonic_ns - target_ns) <= window_ns
    ]
    return min(candidates, key=lambda sample: abs(sample.monotonic_ns - target_ns), default=None)


def export_diffs(capture_path: str, out_dir: str) -> int:
    metadata, samples = load_capture(capture_path)
    poll_rate_ms = int(metadata.get("poll_rate_ms", 150))
    window_ns = _websocket_barrier_window_ns(poll_rate_ms)
    v2_http = {
        daemon: sorted(
            [sample for sample in samples
             if sample.daemon == daemon
             and sample.schema == "v2"
             and sample.transport == "http"],
            key=lambda sample: sample.monotonic_ns,
        )
        for daemon in ("source", "vosu")
    }
    contexts = _ordered_barrier_contexts(
        v2_http, poll_rate_ms, metadata.get("barrier_times"))
    root = Path(out_dir)
    root.mkdir(parents=True, exist_ok=True)
    manifest: list[dict[str, Any]] = []
    combined_diffs: list[str] = []

    for barrier in _BARRIERS:
        left_context = contexts["source"].get(barrier)
        right_context = contexts["vosu"].get(barrier)
        if left_context is None or right_context is None:
            continue
        for transport in _TRANSPORTS:
            for schema in SCHEMAS:
                left = _nearest(
                    samples, "source", schema, transport,
                    left_context.monotonic_ns, window_ns)
                right = _nearest(
                    samples, "vosu", schema, transport,
                    right_context.monotonic_ns, window_ns)
                if left is None or right is None:
                    continue
                stem = f"{barrier}.{transport}.{schema}"
                left_text = _canonical(left)
                right_text = _canonical(right)
                (root / f"{stem}.source.json").write_text(left_text, encoding="utf-8")
                (root / f"{stem}.vosu.json").write_text(right_text, encoding="utf-8")
                diff = "".join(difflib.unified_diff(
                    left_text.splitlines(keepends=True),
                    right_text.splitlines(keepends=True),
                    fromfile=f"{stem}.source.json",
                    tofile=f"{stem}.vosu.json",
                ))
                (root / f"{stem}.diff").write_text(diff, encoding="utf-8")
                if diff:
                    combined_diffs.append(diff)
                manifest.append({
                    "barrier": barrier,
                    "transport": transport,
                    "schema": schema,
                    "identical": left_text == right_text,
                    "source_monotonic_ns": left.monotonic_ns,
                    "vosu_monotonic_ns": right.monotonic_ns,
                    "delta_ms": abs(left.monotonic_ns - right.monotonic_ns) / 1_000_000,
                    "diff": f"{stem}.diff",
                })

    (root / "all.diff").write_text(
        "\n".join(combined_diffs),
        encoding="utf-8",
    )

    (root / "manifest.json").write_text(
        json.dumps({
            "capture": str(Path(capture_path).resolve()),
            "window_ms": window_ns / 1_000_000,
            "pairs": manifest,
        }, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    differing = sum(not item["identical"] for item in manifest)
    print(json.dumps({
        "pairs": len(manifest),
        "differing": differing,
        "out_dir": str(root.resolve()),
    }, sort_keys=True))
    return 0
