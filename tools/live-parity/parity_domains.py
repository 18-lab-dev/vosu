"""Reference capability, path-domain, segmentation, and cadence policy."""
from __future__ import annotations

import math
from collections.abc import Iterable
from typing import Any


# These are fields whose values require the native GameBase object or calculations
# derived from it. Shared IPC/result identity and hit-error retention deliberately
# do not appear here.
GAMEBASE_PATH_FAMILIES: dict[str, tuple[tuple[str, ...], ...]] = {
    "v1": (
        ("settings", "folders", "songs"),
        ("menu", "bm", "stats"),
        ("menu", "bm", "time", "firstObj"),
        ("menu", "bm", "time", "lastObj"),
        ("menu", "bm", "time", "full"),
        ("menu", "mods"),
        ("menu", "pp"),
        ("gameplay", "accuracy"),
        ("gameplay", "combo"),
        ("gameplay", "gameMode"),
        ("gameplay", "hits", "grade"),
        ("gameplay", "hp"),
        ("gameplay", "leaderboard"),
        ("gameplay", "name"),
        ("gameplay", "pp"),
    ),
    "v2": (
        ("folders", "songs"),
        ("beatmap", "isConvert"),
        ("beatmap", "mode"),
        ("beatmap", "stats"),
        ("beatmap", "tags"),
        ("beatmap", "time", "firstObject"),
        ("beatmap", "time", "lastObject"),
        ("performance",),
        ("play", "accuracy"),
        ("play", "combo"),
        ("play", "healthBar"),
        ("play", "mode"),
        ("play", "mods"),
        ("play", "playerName"),
        ("play", "pp"),
        ("play", "rank"),
        ("leaderboard",),
        ("resultsScreen", "pp"),
    ),
    "sc": (
        ("ar",), ("bpm",), ("circles",), ("cs",), ("currentBpm",),
        ("drainingtime",), ("firstHitObjectTime",), ("gameMode",),
        ("grade",), ("hp",), ("leaderBoardMainPlayer",),
        ("leaderBoardPlayers",), ("liveStarRating",), ("mAR",), ("mBpm",),
        ("mCS",), ("mHP",), ("mMainBpm",), ("mMaxBpm",), ("mMinBpm",),
        ("mOD",), ("mStars",), ("mainBpm",), ("mapBreaks",),
        ("mapKiaiPoints",), ("mapStrains",), ("mapTimingPoints",),
        ("maxBpm",), ("maxCombo",), ("maxGrade",), ("minBpm",), ("mode",),
        ("mods",), ("modsEnum",), ("od",), ("playerHp",),
        ("playerHpSmooth",), ("previewtime",), ("stars",), ("totaltime",),
        ("username",),
    ),
}


def gamebase_path_family(schema: str, parts: tuple[str, ...]) -> str | None:
    """Return the registered family containing *parts*, if any."""
    if (schema == "sc" and parts and len(parts) == 1
            and "pp" in parts[0].lower()):
        return "/pp-calculation"
    for prefix in GAMEBASE_PATH_FAMILIES.get(schema, ()):
        if parts[:len(prefix)] == prefix:
            return "/" + "/".join(prefix)
    return None


def _finite_number(value: Any) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool) \
        and math.isfinite(value)


def _v2_sentinel(payload: Any) -> dict[str, Any] | None:
    if not isinstance(payload, dict):
        return None
    beatmap = payload.get("beatmap")
    timing = beatmap.get("time") if isinstance(beatmap, dict) else None
    checksum = beatmap.get("checksum") if isinstance(beatmap, dict) else None
    first = timing.get("firstObject") if isinstance(timing, dict) else None
    last = timing.get("lastObject") if isinstance(timing, dict) else None
    folders = payload.get("folders")
    songs = folders.get("songs") if isinstance(folders, dict) else None
    graph = payload.get("performance")
    graph = graph.get("graph") if isinstance(graph, dict) else None
    series = graph.get("series") if isinstance(graph, dict) else None
    valid_checksum = isinstance(checksum, str) and bool(checksum.strip())
    if not valid_checksum or not _finite_number(first) or not _finite_number(last):
        return None
    empty_graph = not series or all(
        not isinstance(item, dict) or not item.get("data") for item in series
    ) if isinstance(series, list) else not series
    return {
        "checksum": checksum,
        "first_object": first,
        "last_object": last,
        "songs_empty": not isinstance(songs, str) or not songs.strip(),
        "graph_empty": bool(empty_graph),
        "sentinel": first == 0 and last == 0
                    and (not isinstance(songs, str) or not songs.strip())
                    and bool(empty_graph),
    }


def infer_reference_capabilities(samples: Iterable[Any]) -> dict[str, Any]:
    """Infer GameBase observability from successful source V2 observations."""
    evidence = []
    for sample in samples:
        if (getattr(sample, "daemon", None) != "source"
                or getattr(sample, "schema", None) != "v2"
                or getattr(sample, "status", None) != 200
                or getattr(sample, "error", None) is not None):
            continue
        item = _v2_sentinel(getattr(sample, "payload", None))
        if item is not None:
            evidence.append(item)
    ready = [item for item in evidence if not item["sentinel"]]
    sentinels = [item for item in evidence if item["sentinel"]]
    if ready:
        status = "ready"
        reason = "non-default GameBase boundary evidence observed"
    elif len(sentinels) >= 2:
        status = "unresolved"
        reason = "repeated valid-checksum zero-boundary/empty-songs/empty-graph sentinel"
    else:
        status = "unknown"
        reason = "insufficient synchronized V2 sentinel evidence"
    return {
        "gamebase": {
            "status": status,
            "reason": reason,
            "observations": len(evidence),
            "sentinel_observations": len(sentinels),
            "checksums": sorted({item["checksum"] for item in evidence}),
        }
    }


def confirmed_retry_boundaries(source: Iterable[Any], vosu: Iterable[Any],
                               window_ns: int, time_of: Any) -> dict[str, list[int]]:
    """Find backward-clock resets corroborated by both daemons in the time window."""
    drops: dict[str, list[int]] = {"source": [], "vosu": []}
    for daemon, stream in (("source", source), ("vosu", vosu)):
        previous = None
        for sample in sorted(stream, key=lambda item: item.monotonic_ns):
            current = time_of(sample.payload)
            if current is not None and previous is not None and current < previous:
                drops[daemon].append(sample.monotonic_ns)
            if current is not None:
                previous = current
    confirmed = {"source": [], "vosu": []}
    unused = set(range(len(drops["vosu"])))
    for source_time in drops["source"]:
        choices = [(abs(source_time - drops["vosu"][index]), index)
                   for index in unused
                   if abs(source_time - drops["vosu"][index]) <= window_ns]
        if choices:
            _, index = min(choices)
            unused.remove(index)
            confirmed["source"].append(source_time)
            confirmed["vosu"].append(drops["vosu"][index])
    return confirmed


def segment_at(timestamp: int, boundaries: Iterable[int]) -> int:
    return sum(boundary <= timestamp for boundary in boundaries)


def cadence_evidence(points: Iterable[int], maximum_ms: float) -> dict[str, Any]:
    ordered = sorted(set(points))
    gaps_ms = [(later - earlier) / 1_000_000
               for earlier, later in zip(ordered, ordered[1:])]
    return {
        "observations": len(ordered),
        "nominal_rate_hz": (1000.0 / (sum(gaps_ms) / len(gaps_ms))) if gaps_ms else None,
        "maximum_gap_ms": max(gaps_ms, default=None),
        "stall_count": sum(gap > maximum_ms for gap in gaps_ms),
        "stall_threshold_ms": maximum_ms,
    }
