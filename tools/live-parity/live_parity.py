#!/usr/bin/env python3
"""Deterministic live vosu parity capture and comparison primitives."""
from __future__ import annotations

from bisect import bisect_left, bisect_right
from dataclasses import dataclass, asdict
import json
import math
import time
from typing import Any, Callable, Iterable

SCHEMAS = {
    "v1": "/json",
    "v2": "/json/v2",
    "precise": "/json/v2/precise",
    "sc": "/json/sc",
}
PINNED_SOURCE_REVISION = "2359305a63f2aaebb201bc8d2eee58177f52590e"
PINNED_SOURCE_VERSION = "4.26.2"
PINNED_VOSU_VERSION = "1.1.0"
SOURCE_BUILD_LABEL = (
    f"upstream-tosu-source-v{PINNED_SOURCE_VERSION}@{PINNED_SOURCE_REVISION[:12]}"
)
VOSU_BUILD_LABEL = f"vosu-v{PINNED_VOSU_VERSION}"
SUPPORTED_MATRIX = {("windows", "stable"), ("windows", "lazer"), ("linux", "lazer")}
PLAY_STATES = {"play", "playing", 2}
MENU_STATES = {"menu", "selectplay", 0, 5}
RESULT_STATES = {"result", "results", "resultscreen", "resultScreen", 7}


@dataclass(frozen=True)
class Sample:
    daemon: str
    schema: str
    monotonic_ns: int
    payload: Any | None
    status: int = 200
    error: str | None = None
    transport: str = "http"

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "Sample":
        return cls(**value)


@dataclass(frozen=True)
class Mismatch:
    schema: str
    pointer: str
    comparator: str
    source: Any
    vosu: Any
    source_monotonic_ns: int
    vosu_monotonic_ns: int
    detail: str
    tolerance: float | None = None


def json_pointer(parts: Iterable[str]) -> str:
    encoded = (p.replace("~", "~0").replace("/", "~1") for p in parts)
    return "/" + "/".join(encoded)


def state_of(payload: Any) -> Any:
    if not isinstance(payload, dict):
        return None
    state = payload.get("state")
    if isinstance(state, dict):
        return state.get("name", state.get("number"))
    if state is not None:
        return state
    menu = payload.get("menu")
    if isinstance(menu, dict):
        return menu.get("state")
    return None


def paused_of(payload: Any) -> bool:
    if not isinstance(payload, dict):
        return False
    game = payload.get("game")
    if isinstance(game, dict) and isinstance(game.get("paused"), bool):
        return game["paused"]
    gameplay = payload.get("gameplay")
    return bool(gameplay.get("paused")) if isinstance(gameplay, dict) else False


def beatmap_time(payload: Any) -> float | None:
    if not isinstance(payload, dict):
        return None
    candidates = [
        ("beatmap", "time", "live"), ("session", "playTime"),
        ("menu", "bm", "time", "current"),
    ]
    for path in candidates:
        cur: Any = payload
        for key in path:
            if not isinstance(cur, dict) or key not in cur:
                break
            cur = cur[key]
        else:
            if isinstance(cur, (int, float)) and not isinstance(cur, bool) and math.isfinite(cur):
                return float(cur)
    return None


def barrier_of(sample: Sample) -> str | None:
    if sample.status == 500 or sample.error == "not_ready":
        return "process-exit"
    state = state_of(sample.payload)
    normalized = state.lower() if isinstance(state, str) else state
    if normalized in MENU_STATES:
        return "menu"
    if normalized in RESULT_STATES:
        return "result"
    if normalized in PLAY_STATES:
        return "paused" if paused_of(sample.payload) else "play"
    return None


def validate_barriers(samples: list[Sample], required: tuple[str, ...] = ("menu", "play", "paused", "result", "process-exit")) -> list[str]:
    """Return missing/out-of-order barriers. Repeated or unrelated samples are ignored."""
    position = 0
    for sample in sorted(samples, key=lambda s: s.monotonic_ns):
        barrier = barrier_of(sample)
        if position < len(required) and barrier == required[position]:
            position += 1
    return list(required[position:])


def align_active(
        source: list[Sample], vosu: list[Sample], poll_rate_ms: int,
        source_segment: Callable[[Sample], object] | None = None,
        vosu_segment: Callable[[Sample], object] | None = None,
) -> tuple[list[tuple[Sample, Sample]], list[Sample]]:
    """Maximize unique ordered matches, then minimize clock and receipt skew."""
    window = max(2 * poll_rate_ms, 300)
    source_groups: dict[object, list[Sample]] = {}
    vosu_groups: dict[object, list[Sample]] = {}
    for values, partition, groups in (
            (source, source_segment, source_groups),
            (vosu, vosu_segment, vosu_groups)):
        for sample in sorted(values, key=lambda s: s.monotonic_ns):
            segment = partition(sample) if partition is not None else 0
            groups.setdefault(segment, []).append(sample)
    pairs: list[tuple[Sample, Sample]] = []
    unaligned: list[Sample] = []
    for segment, left in source_groups.items():
        matched, missing = _align_clock_segment(
            left, vosu_groups.get(segment, []), window)
        pairs.extend(matched)
        unaligned.extend(missing)
    pairs.sort(key=lambda pair: pair[0].monotonic_ns)
    unaligned.sort(key=lambda sample: sample.monotonic_ns)
    return pairs, unaligned


_MAX_ALIGNMENT_CANDIDATES = 1_000_000


def _align_clock_segment(
        source: list[Sample], vosu: list[Sample], window: int,
) -> tuple[list[tuple[Sample, Sample]], list[Sample]]:
    by_state: dict[object, list[tuple[float, int]]] = {}
    for index, sample in enumerate(vosu):
        clock = beatmap_time(sample.payload)
        if clock is not None:
            by_state.setdefault(state_of(sample.payload), []).append((clock, index))
    for candidates in by_state.values():
        candidates.sort()

    # Sparse ordered matching: a Fenwick prefix tree stores best chains.
    scores: list[tuple[int, float, int]] = [(0, 0.0, 0)]
    links: list[tuple[int, int, int]] = [(-1, -1, -1)]
    tree = [0] * (len(vosu) + 1)
    best = 0
    for left_index, left in enumerate(source):
        clock = beatmap_time(left.payload)
        if clock is None:
            continue
        candidates = by_state.get(state_of(left.payload), [])
        start = bisect_left(candidates, (clock - window, -1))
        end = bisect_right(candidates, (clock + window, len(vosu)))
        candidate_count = end - start

        if (candidate_count
                and len(scores) - 1 + candidate_count
                > _MAX_ALIGNMENT_CANDIDATES):
            raise ValueError("active alignment exceeds the bounded candidate budget")
        pending: list[int] = []
        for candidate in range(start, end):
            right_clock, right_index = candidates[candidate]
            if (paused_of(vosu[right_index].payload)
                    and abs(left.monotonic_ns - vosu[right_index].monotonic_ns)
                    > window * 1_000_000):
                continue
            previous = 0
            position = right_index
            while position:
                if scores[tree[position]] > scores[previous]:
                    previous = tree[position]
                position -= position & -position
            count, clock_skew, receipt_skew = scores[previous]
            score = (
                count + 1,
                clock_skew - abs(clock - right_clock),
                receipt_skew - abs(left.monotonic_ns - vosu[right_index].monotonic_ns),
            )
            node = len(scores)
            scores.append(score)
            links.append((previous, left_index, right_index))
            pending.append(node)
            if score > scores[best]:
                best = node
        for node in pending:
            position = links[node][2] + 1
            while position < len(tree):
                if scores[node] > scores[tree[position]]:
                    tree[position] = node
                position += position & -position
    pairs: list[tuple[Sample, Sample]] = []
    matched_source: set[int] = set()
    while best:
        best, left_index, right_index = links[best]
        pairs.append((source[left_index], vosu[right_index]))
        matched_source.add(left_index)
    pairs.reverse()
    return pairs, [
        sample for index, sample in enumerate(source)
        if index not in matched_source
    ]


def _kind(value: Any) -> str:
    if value is None: return "null"
    if isinstance(value, bool): return "boolean"
    if isinstance(value, (int, float)): return "number"
    if isinstance(value, str): return "string"
    if isinstance(value, list): return "array"
    if isinstance(value, dict): return "object"
    return type(value).__name__


def _is_volatile(schema: str, parts: tuple[str, ...], active: bool) -> bool:
    always_volatile = {
        "v1": {
            ("menu", "mainMenu", "bassDensity"),
            ("menu", "bm", "time", "current"),
            ("gameplay", "paused"),
            ("settings", "showInterface"),
        },
        "v2": {
            ("settings", "bassDensity"),
            ("settings", "interfaceVisible"),
            ("session", "playTime"),
            ("beatmap", "time", "live"),
            ("game", "paused"),
        },
        "sc": {
            ("mapPosition",),
            ("time",),
            ("timeLeft",),
        },
        "precise": set(),
    }[schema]
    if any(parts[:len(root)] == root for root in always_volatile):
        return True
    if not active:
        return False
    if schema in {"sc", "precise"}:
        return True
    roots = {
        "v1": {
            ("menu", "bm", "stats", "SR"),
            ("gameplay",),
            ("resultsScreen", "mode"),
        },
        "v2": {
            ("beatmap", "stats", "stars", "live"),
            ("play",),
        },
    }[schema]
    return any(parts[:len(root)] == root for root in roots)


def _is_hit_error_sequence(schema: str, parts: tuple[str, ...]) -> bool:
    return parts == {
        "precise": ("hitErrors",),
        "sc": ("hitErrors",),
        "v1": ("gameplay", "hits", "hitErrorArray"),
        "v2": ("play", "hitErrorArray"),
    }[schema]


def _is_dynamic_sequence(schema: str, parts: tuple[str, ...], active: bool) -> bool:
    return active and _is_hit_error_sequence(schema, parts)

def _is_graph_float(schema: str, parts: tuple[str, ...]) -> bool:
    return (
        schema == "v1"
        and (
            (len(parts) == 4 and parts[:3] == ("menu", "pp", "strains"))
            or (
                len(parts) == 7
                and parts[:4] == ("menu", "pp", "strainsAll", "series")
                and parts[5] == "data"
            )
        )
        and parts[-1].isdigit()
    ) or (
        schema == "v2"
        and len(parts) == 6
        and parts[:3] == ("performance", "graph", "series")
        and parts[4] == "data"
        and parts[-1].isdigit()
    ) or (
        schema == "sc"
        and len(parts) == 2
        and parts[0] == "mapStrains"
        and parts[1].isdigit()
    )


def _within_one_ulp(left: float, right: float) -> bool:
    return (
        math.isfinite(left)
        and math.isfinite(right)
        and (left == right or math.nextafter(left, right) == right)
    )


def _is_vosu_extension(schema: str, parts: tuple[str, ...]) -> bool:
    """Exact additive API paths documented in CHANGELOG 1.0.0, not wildcards."""
    if schema == "precise":
        if len(parts) >= 3 and parts[0] == "tourney" and parts[1].isdigit():
            parts = parts[2:]
        return parts in {
            ("pressHitErrors",), ("releaseHitErrors",), ("keys", "all")}
    if schema == "v2":
        if (
                len(parts) == 5 and parts[:2] == ("tourney", "clients")
                and parts[2].isdigit() and parts[3] == "play"):
            parts = parts[3:]
        return parts in {
            ("play", "pressHitErrors"), ("play", "releaseHitErrors"),
            ("keys",), ("keys", "all")}
    if schema == "v1":
        if (
                len(parts) == 6 and parts[:2] == ("tourney", "ipcClients")
                and parts[2].isdigit()):
            parts = parts[3:]
        return parts in {
            ("gameplay", "hits", "pressHitErrors"),
            ("gameplay", "hits", "releaseHitErrors")}
    if schema == "sc":
        return parts in {("pressHitErrors",), ("releaseHitErrors",)}
    return False



def compare_pair(
        left: Sample, right: Sample, alignment_window_ms: int,
        active_override: bool | None = None,
        unobservable: Callable[[str, tuple[str, ...]], str | None] | None = None,
        coverage: list[dict[str, Any]] | None = None,
) -> list[Mismatch]:
    mismatches: list[Mismatch] = []
    active = (state_of(left.payload) in PLAY_STATES and not paused_of(left.payload)) if active_override is None else active_override

    def excluded(parts: tuple[str, ...]) -> bool:
        family = unobservable(left.schema, parts) if unobservable is not None else None
        if family is None:
            return False
        if coverage is not None:
            coverage.append({
                "schema": left.schema,
                "path_family": family,
                "reason": "reference_gamebase_unresolved",
                "source_monotonic_ns": left.monotonic_ns,
                "vosu_monotonic_ns": right.monotonic_ns,
            })
        return True
    def has_observable(value: Any, parts: tuple[str, ...]) -> bool:
        if excluded(parts):
            return False
        if isinstance(value, dict) and value:
            return any(
                has_observable(child, parts + (key,))
                for key, child in value.items())
        if isinstance(value, list) and value:
            return any(
                has_observable(child, parts + (str(index),))
                for index, child in enumerate(value))
        return True


    def add(
            parts: tuple[str, ...], comparator: str, a: Any, b: Any,
            detail: str, tolerance: float | None = None) -> None:
        if comparator == "aligned-time":
            tolerance = float(alignment_window_ms)
        mismatches.append(Mismatch(
            left.schema, json_pointer(parts) if parts else "", comparator, a, b,
            left.monotonic_ns, right.monotonic_ns, detail, tolerance))

    def walk(a: Any, b: Any, parts: tuple[str, ...]) -> None:
        if excluded(parts):
            return
        if (
                _is_hit_error_sequence(left.schema, parts)
                and isinstance(a, list)
                and isinstance(b, dict)
                and isinstance(b.get("all"), list)):
            b = b["all"]
        ka, kb = _kind(a), _kind(b)
        if ka != kb:
            add(parts, "type", a, b, f"type differs: {ka} != {kb}")
            return
        volatile = _is_volatile(left.schema, parts, active)
        if isinstance(a, dict):
            observable_a = {
                key for key, value in a.items()
                if has_observable(value, parts + (key,))}
            observable_b = {
                key for key, value in b.items()
                if (key in a or not _is_vosu_extension(left.schema, parts + (key,)))
                and has_observable(value, parts + (key,))}
            if observable_a != observable_b:
                add(parts, "keys", sorted(observable_a), sorted(observable_b),
                    "object key set differs")
            for key in sorted(observable_a & observable_b):
                walk(a[key], b[key], parts + (key,))
        elif isinstance(a, list):
            if _is_dynamic_sequence(left.schema, parts, active):
                return
            if len(a) != len(b):
                add(parts, "length", len(a), len(b), "array length differs")
            for idx, (av, bv) in enumerate(zip(a, b)):
                walk(av, bv, parts + (str(idx),))
        elif volatile:
            if (active and parts in {
                    ("menu", "bm", "time", "current"),
                    ("session", "playTime"),
                    ("beatmap", "time", "live")}):
                if isinstance(a, (int, float)) and abs(a - b) > alignment_window_ms:
                    add(parts, "aligned-time", a, b, f"difference exceeds {alignment_window_ms} ms")
        elif (
                a != b
                and _is_graph_float(left.schema, parts)
                and isinstance(a, float)
                and isinstance(b, float)
                and _within_one_ulp(a, b)):
            return
        elif a != b:
            add(parts, "exact", a, b, "values differ")

    if left.status != right.status:
        add((), "http-status", left.status, right.status, "HTTP status differs")
    elif left.error != right.error:
        add((), "transport-error", left.error, right.error, "transport outcome differs")
    else:
        walk(left.payload, right.payload, ())
    return mismatches


def monotonic_violations(
        samples: list[Sample], is_active: Callable[[Sample], bool] | None = None,
        segment_of: Callable[[Sample], object] | None = None,
) -> list[Mismatch]:
    """Enforce nondecreasing active values within each confirmed play segment."""
    out: list[Mismatch] = []
    previous: dict[tuple[str, str, str, object], dict[str, tuple[float, Sample]]] = {}
    counts: dict[tuple[str, str, str], dict[str, int]] = {}
    active_streams: set[tuple[str, str, str]] = set()
    hit_names = (
        "0", "50", "100", "300", "geki", "katu", "sliderBreaks",
        "sliderEndHits", "smallTickHits", "largeTickHits",
    )
    v1_paths = {
        ("menu", "bm", "time", "current"), ("gameplay", "score"),
        *(("gameplay", "hits", name) for name in hit_names),
    }
    v2_paths = {
        ("session", "playTime"), ("beatmap", "time", "live"), ("play", "score"),
        *(("play", "hits", name) for name in hit_names),
    }
    sc_paths = {
        (name,) for name in (
            "time", "score", "geki", "c300", "katsu", "c100", "c50", "miss",
            "sliderBreaks",
        )
    }
    precise_paths = {
        ("keys", key, "count") for key in ("k1", "k2", "m1", "m2")
    }
    selected = {
        "v1": v1_paths, "v2": v2_paths, "sc": sc_paths,
        "precise": precise_paths,
    }

    def leaves(value: Any, parts: tuple[str, ...] = ()) -> Iterable[tuple[tuple[str, ...], float]]:
        if isinstance(value, dict):
            for key, child in value.items():
                yield from leaves(child, parts + (key,))
        elif isinstance(value, (int, float)) and not isinstance(value, bool):
            yield parts, float(value)

    for sample in sorted(samples, key=lambda s: s.monotonic_ns):
        active = is_active(sample) if is_active is not None else (
            state_of(sample.payload) in PLAY_STATES and not paused_of(sample.payload))
        base_stream = (sample.daemon, sample.schema, sample.transport)
        segment = segment_of(sample) if segment_of is not None else 0
        stream = (*base_stream, segment)
        if not active:
            previous.pop(stream, None)
            continue
        active_streams.add(base_stream)
        for parts, value in leaves(sample.payload):
            if parts not in selected[sample.schema]:
                continue
            pointer = json_pointer(parts)
            if not math.isfinite(value):
                out.append(Mismatch(
                    sample.schema, pointer, "invalid-numeric", value, value,
                    sample.monotonic_ns, sample.monotonic_ns,
                    "selected active numeric value is not finite", None))
                continue
            counts.setdefault(base_stream, {})[pointer] = counts.setdefault(base_stream, {}).get(pointer, 0) + 1
            stream_values = previous.setdefault(stream, {})
            prior = stream_values.get(pointer)
            if prior and value < prior[0]:
                old, old_sample = prior
                out.append(Mismatch(sample.schema, pointer, "monotonic", old, value,
                                    old_sample.monotonic_ns, sample.monotonic_ns,
                                    "active value decreased", 0.0))
            stream_values[pointer] = (value, sample)

    for stream in sorted(active_streams):
        daemon, schema, transport = stream
        for parts in sorted(selected[schema]):
            pointer = json_pointer(parts)
            count = counts.get(stream, {}).get(pointer, 0)
            if count < 2:
                out.append(Mismatch(
                    schema, pointer, "coverage", 2, count, 0, 0,
                    f"{daemon}/{transport} requires at least two numeric active observations", None))

    return out

def dump_capture(path: str, samples: list[Sample], metadata: dict[str, Any]) -> None:
    with open(path, "w", encoding="utf-8") as fh:
        json.dump({"format": 1, "metadata": metadata, "samples": [asdict(s) for s in samples]}, fh,
                  indent=2, sort_keys=True, ensure_ascii=False)
        fh.write("\n")


def load_capture(path: str) -> tuple[dict[str, Any], list[Sample]]:
    with open(path, encoding="utf-8") as fh:
        value = json.load(fh)
    if value.get("format") != 1 or not isinstance(value.get("samples"), list):
        raise ValueError(f"unsupported capture format: {path}")
    return value.get("metadata", {}), [Sample.from_dict(v) for v in value["samples"]]
