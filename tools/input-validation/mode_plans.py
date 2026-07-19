from __future__ import annotations

import math
from dataclasses import dataclass
from decimal import Decimal, InvalidOperation, ROUND_CEILING, ROUND_HALF_UP
from pathlib import Path
from typing import TypeAlias

from input_validation import ValidationError


_MAX_OBJECTS = 100_000
_MAX_ACTIONS = 200_000
_MAX_SAMPLES_PER_OBJECT = 10_000
_MAX_TIMESTAMP_MS = 86_400_000
_MAX_REPEAT_COUNT = 10_000

_MAX_CURVE_CONTROL_POINTS = 64
_MAX_CURVE_POLYLINE_POINTS = 10_000

@dataclass(frozen=True, order=True)
class KeyAction:
    timestamp_ms: int
    control: str
    pressed: bool


@dataclass(frozen=True, order=True)
class PointerMove:
    timestamp_ms: int
    x: float
    y: float


Action: TypeAlias = KeyAction | PointerMove


@dataclass(frozen=True, order=True)
class PlayPlan:
    mode: int
    controls: tuple[str, ...]
    actions: tuple[Action, ...]


@dataclass(frozen=True)
class _TimingPoint:
    time: Decimal
    beat_length: Decimal
    uninherited: bool


@dataclass(frozen=True)
class _Object:
    x: int
    y: int
    start: int
    kind: str
    hitsound: int
    end: int | None = None
    repeats: int = 1
    length: Decimal | None = None
    curve_type: str = ""
    points: tuple[tuple[float, float], ...] = ()




def _fail(message: str) -> ValidationError:
    return ValidationError(message)


def _integer(value: str, description: str, location: str) -> int:
    try:
        parsed = int(value.strip())
    except ValueError:
        raise _fail(f"invalid {description} at {location}") from None
    return parsed


def _decimal(value: str, description: str, location: str) -> Decimal:
    try:
        parsed = Decimal(value.strip())
    except InvalidOperation:
        raise _fail(f"invalid {description} at {location}") from None
    if not parsed.is_finite():
        raise _fail(f"invalid {description} at {location}")
    return parsed


def _rounded(value: Decimal) -> int:
    return int(value.to_integral_value(rounding=ROUND_HALF_UP))


def _check_timestamp(value: int, description: str, location: str) -> int:
    if not 0 <= value <= _MAX_TIMESTAMP_MS:
        raise _fail(f"invalid {description} at {location}")
    return value


def _parse_slider(fields: list[str], x: int, y: int, start: int, hitsound: int, location: str) -> _Object:
    if len(fields) < 8:
        raise _fail(f"invalid slider at {location}")
    curve = fields[5].split("|")
    if len(curve) < 2 or curve[0] not in {"B", "C", "L", "P"}:
        raise _fail(f"invalid slider path at {location}")
    if len(curve) > _MAX_CURVE_CONTROL_POINTS:
        raise _fail(f"slider has too many control points at {location}")
    points: list[tuple[float, float]] = [(float(x), float(y))]
    for encoded in curve[1:]:
        coordinate = encoded.split(":")
        if len(coordinate) != 2:
            raise _fail(f"invalid slider path at {location}")
        px = _integer(coordinate[0], "slider x coordinate", location)
        py = _integer(coordinate[1], "slider y coordinate", location)
        if not 0 <= px <= 512 or not 0 <= py <= 384:
            raise _fail(f"invalid slider coordinate at {location}")
        points.append((float(px), float(py)))
    repeats = _integer(fields[6], "slider repeat count", location)
    if not 1 <= repeats <= _MAX_REPEAT_COUNT:
        raise _fail(f"invalid slider repeat count at {location}")
    length = _decimal(fields[7], "slider length", location)
    if not Decimal("0.001") <= length <= Decimal(1_000_000):
        raise _fail(f"invalid slider length at {location}")
    return _Object(
        x,
        y,
        start,
        "slider",
        hitsound,
        repeats=repeats,
        length=length,
        curve_type=curve[0],
        points=tuple(points),
    )


def _parse_object(line: str, mode: int, location: str) -> _Object:
    fields = line.split(",")
    if len(fields) < 5:
        raise _fail(f"invalid hit object at {location}")
    x = _integer(fields[0], "hit object x coordinate", location)
    y = _integer(fields[1], "hit object y coordinate", location)
    start = _check_timestamp(_integer(fields[2], "hit object time", location), "hit object time", location)
    object_type = _integer(fields[3], "hit object type", location)
    hitsound = _integer(fields[4], "hit sound", location)
    if not 0 <= x <= 512 or not 0 <= y <= 384 or not 0 <= hitsound <= 15 or not 0 <= object_type <= 255:
        raise _fail(f"invalid hit object at {location}")
    base_flags = object_type & (1 | 2 | 8 | 128)
    if base_flags == 0 or base_flags & (base_flags - 1):
        raise _fail(f"unsupported hit object type {object_type} at {location}")
    if base_flags == 1:
        if mode == 3:
            return _Object(x, y, start, "mania-circle", hitsound)
        return _Object(x, y, start, "circle", hitsound)
    if base_flags == 128:
        if mode != 3 or len(fields) < 6:
            raise _fail(f"unsupported hit object type {object_type} at {location}")
        end_text = fields[5].split(":", 1)[0]
        end = _check_timestamp(_integer(end_text, "hold end time", location), "hold end time", location)
        if end <= start:
            raise _fail(f"hold must end after it starts at {location}")
        return _Object(x, y, start, "hold", hitsound, end=end)
    if mode == 3:
        raise _fail(f"unsupported hit object type {object_type} at {location}")
    if base_flags == 2:
        return _parse_slider(fields, x, y, start, hitsound, location)
    if len(fields) < 6:
        raise _fail(f"spinner has no end time at {location}")
    end = _check_timestamp(_integer(fields[5], "spinner end time", location), "spinner end time", location)
    if end <= start:
        raise _fail(f"spinner must end after it starts at {location}")
    return _Object(x, y, start, "spinner", hitsound, end=end)


def _timing_at(points: list[_TimingPoint], timestamp: int, location: str) -> tuple[Decimal, Decimal]:
    beat: Decimal | None = None
    velocity = Decimal(1)
    instant = Decimal(timestamp)
    for point in points:
        if point.time > instant:
            break
        if point.uninherited:
            beat = point.beat_length
            velocity = Decimal(1)
        else:
            velocity = -Decimal(100) / point.beat_length
    if beat is None:
        raise _fail(f"no uninherited timing point at or before slider at {location}")
    return beat, velocity


def _slider_duration(obj: _Object, timings: list[_TimingPoint], multiplier: Decimal, location: str) -> int:
    beat, velocity = _timing_at(timings, obj.start, location)
    assert obj.length is not None
    duration = obj.length * beat * obj.repeats / (multiplier * Decimal(100) * velocity)
    milliseconds = _rounded(duration)
    if milliseconds <= 0 or obj.start + milliseconds > _MAX_TIMESTAMP_MS:
        raise _fail(f"invalid slider duration at {location}")
    return milliseconds


def _bezier_point(points: list[tuple[float, float]], progress: float) -> tuple[float, float]:
    work = list(points)
    for remaining in range(len(work) - 1, 0, -1):
        for index in range(remaining):
            x1, y1 = work[index]
            x2, y2 = work[index + 1]
            work[index] = (x1 + (x2 - x1) * progress, y1 + (y2 - y1) * progress)
    return work[0]


def _bezier_path(points: tuple[tuple[float, float], ...]) -> list[tuple[float, float]]:
    segments: list[list[tuple[float, float]]] = []
    segment = [points[0]]
    for point in points[1:]:
        if point == segment[-1] and len(segment) > 1:
            segments.append(segment)
            segment = [point]
        else:
            segment.append(point)
    if len(segment) > 1:
        segments.append(segment)
    path: list[tuple[float, float]] = []
    for controls in segments:
        subdivisions = max(16, min(128, len(controls) * 16))
        for index in range(subdivisions + 1):
            if path and index == 0:
                continue
            path.append(_bezier_point(controls, index / subdivisions))
    return path or [points[0]]


def _catmull_path(points: tuple[tuple[float, float], ...]) -> list[tuple[float, float]]:
    extended = (points[0],) + points + (points[-1],)
    path: list[tuple[float, float]] = []
    subdivisions = 32
    for segment in range(len(points) - 1):
        p0, p1, p2, p3 = extended[segment : segment + 4]
        for index in range(subdivisions + 1):
            if path and index == 0:
                continue
            t = index / subdivisions
            t2 = t * t
            t3 = t2 * t
            x = 0.5 * (
                2 * p1[0]
                + (-p0[0] + p2[0]) * t
                + (2 * p0[0] - 5 * p1[0] + 4 * p2[0] - p3[0]) * t2
                + (-p0[0] + 3 * p1[0] - 3 * p2[0] + p3[0]) * t3
            )
            y = 0.5 * (
                2 * p1[1]
                + (-p0[1] + p2[1]) * t
                + (2 * p0[1] - 5 * p1[1] + 4 * p2[1] - p3[1]) * t2
                + (-p0[1] + 3 * p1[1] - 3 * p2[1] + p3[1]) * t3
            )
            path.append((x, y))
    return path


def _perfect_path(points: tuple[tuple[float, float], ...]) -> list[tuple[float, float]] | None:
    if len(points) != 3:
        return None
    (ax, ay), (bx, by), (cx, cy) = points
    determinant = 2 * (ax * (by - cy) + bx * (cy - ay) + cx * (ay - by))
    if abs(determinant) < 1e-9:
        return None
    a2 = ax * ax + ay * ay
    b2 = bx * bx + by * by
    c2 = cx * cx + cy * cy
    center_x = (a2 * (by - cy) + b2 * (cy - ay) + c2 * (ay - by)) / determinant
    center_y = (a2 * (cx - bx) + b2 * (ax - cx) + c2 * (bx - ax)) / determinant
    start = math.atan2(ay - center_y, ax - center_x)
    middle = math.atan2(by - center_y, bx - center_x)
    finish = math.atan2(cy - center_y, cx - center_x)
    counterclockwise = (finish - start) % (2 * math.pi)
    middle_delta = (middle - start) % (2 * math.pi)
    sweep = counterclockwise if middle_delta <= counterclockwise else counterclockwise - 2 * math.pi
    radius = math.hypot(ax - center_x, ay - center_y)
    subdivisions = max(16, min(256, math.ceil(abs(sweep) * radius / 4)))
    return [
        (
            center_x + radius * math.cos(start + sweep * index / subdivisions),
            center_y + radius * math.sin(start + sweep * index / subdivisions),
        )
        for index in range(subdivisions + 1)
    ]


def _fit_path(points: list[tuple[float, float]], expected_length: float) -> tuple[tuple[float, float], ...]:
    fitted = [points[0]]
    remaining = expected_length
    for start, finish in zip(points, points[1:]):
        segment_length = math.hypot(finish[0] - start[0], finish[1] - start[1])
        if segment_length == 0:
            continue
        if segment_length >= remaining:
            portion = remaining / segment_length
            fitted.append(
                (
                    start[0] + (finish[0] - start[0]) * portion,
                    start[1] + (finish[1] - start[1]) * portion,
                )
            )
            return tuple(fitted)
        fitted.append(finish)
        remaining -= segment_length
    if remaining > 0 and len(fitted) > 1:
        before, last = fitted[-2], fitted[-1]
        segment_length = math.hypot(last[0] - before[0], last[1] - before[1])
        if segment_length > 0:
            fitted.append(
                (
                    last[0] + (last[0] - before[0]) * remaining / segment_length,
                    last[1] + (last[1] - before[1]) * remaining / segment_length,
                )
            )
    return tuple(fitted)


def _slider_path(obj: _Object, location: str) -> tuple[tuple[float, float], ...]:
    if obj.curve_type == "L":
        path = list(obj.points)
    elif obj.curve_type == "C":
        path = _catmull_path(obj.points)
    elif obj.curve_type == "P":
        path = _perfect_path(obj.points) or _bezier_path(obj.points)
    else:
        path = _bezier_path(obj.points)
    if len(path) > _MAX_CURVE_POLYLINE_POINTS:
        raise _fail(f"slider path has too many samples at {location}")
    assert obj.length is not None
    return _fit_path(path, float(obj.length))


def _path_position(points: tuple[tuple[float, float], ...], progress: float) -> tuple[float, float]:
    lengths = [math.hypot(b[0] - a[0], b[1] - a[1]) for a, b in zip(points, points[1:])]
    total = sum(lengths)
    if total == 0:
        return points[0]
    target = min(max(progress, 0.0), 1.0) * total
    traversed = 0.0
    for index, length in enumerate(lengths):
        if target <= traversed + length or index == len(lengths) - 1:
            if length == 0:
                return points[index + 1]
            portion = (target - traversed) / length
            a, b = points[index], points[index + 1]
            return (a[0] + (b[0] - a[0]) * portion, a[1] + (b[1] - a[1]) * portion)
        traversed += length
    return points[-1]


def _sample_times(start: int, end: int, cadence: Decimal, location: str) -> tuple[int, ...]:
    if cadence <= 0:
        raise _fail(f"invalid sample cadence at {location}")
    duration = end - start
    count = int((Decimal(duration) / cadence).to_integral_value(rounding=ROUND_CEILING)) + 1
    if count > _MAX_SAMPLES_PER_OBJECT:
        raise _fail(f"hit object generates too many samples at {location}")
    if count <= 1:
        return (start, end) if end != start else (start,)
    times = [start]
    for index in range(1, count):
        timestamp = start + _rounded(cadence * index)
        if timestamp >= end:
            break
        if timestamp > times[-1]:
            times.append(timestamp)
    if times[-1] != end:
        times.append(end)
    return tuple(times)


def _slider_moves(obj: _Object, end: int, cadence: Decimal, y_override: float | None, location: str) -> list[PointerMove]:
    actions: list[PointerMove] = []
    duration = end - obj.start
    path = _slider_path(obj, location)
    sample_times = set(_sample_times(obj.start, end, cadence, location))
    for repeat in range(1, obj.repeats):
        sample_times.add(obj.start + _rounded(Decimal(duration) * repeat / obj.repeats))
    if len(sample_times) > _MAX_SAMPLES_PER_OBJECT:
        raise _fail(f"hit object generates too many samples at {location}")
    for timestamp in sorted(sample_times):
        overall = (timestamp - obj.start) / duration
        repeated = overall * obj.repeats
        cycle = min(int(repeated), obj.repeats - 1)
        fraction = repeated - cycle
        if timestamp == end:
            fraction = 1.0
            cycle = obj.repeats - 1
        if cycle % 2:
            fraction = 1.0 - fraction
        x, y = _path_position(path, fraction)
        actions.append(PointerMove(timestamp, x, y if y_override is None else y_override))
    return actions


def _append(actions: list[Action], new_actions: list[Action], location: str) -> None:
    if len(actions) + len(new_actions) > _MAX_ACTIONS:
        raise _fail(f"play plan generates too many actions at {location}")
    for action in new_actions:
        if not 0 <= action.timestamp_ms <= _MAX_TIMESTAMP_MS:
            raise _fail(f"action time is out of bounds at {location}")
        if isinstance(action, PointerMove) and (
            not math.isfinite(action.x)
            or not math.isfinite(action.y)
            or not 0 <= action.x <= 512
            or not 0 <= action.y <= 384
        ):
            raise _fail(f"pointer coordinate is out of bounds at {location}")
    actions.extend(new_actions)


def _sort_actions(actions: list[Action]) -> tuple[Action, ...]:
    indexed = list(enumerate(actions))
    def key(item: tuple[int, Action]) -> tuple[int, int, int]:
        index, action = item
        if isinstance(action, PointerMove):
            priority = 0
        else:
            priority = 2 if action.pressed else 1
        return action.timestamp_ms, priority, index
    indexed.sort(key=key)
    return tuple(action for _, action in indexed)


def parse_play_plan(path: Path, tap_duration_ms: int = 40) -> PlayPlan:
    if isinstance(tap_duration_ms, bool) or not isinstance(tap_duration_ms, int) or tap_duration_ms <= 0:
        raise _fail("tap duration must be a positive integer number of milliseconds")
    mode: int | None = None
    circle_size: int | None = None
    slider_multiplier: Decimal | None = None
    slider_tick_rate: Decimal | None = None
    timings: list[_TimingPoint] = []
    object_lines: list[tuple[str, str]] = []
    section = ""
    seen: set[tuple[str, str]] = set()
    previous_timing: Decimal | None = None
    try:
        with path.open("r", encoding="utf-8-sig", errors="strict", newline=None) as stream:
            for line_number, raw_line in enumerate(stream, 1):
                line = raw_line.strip()
                if not line or line.startswith("//"):
                    continue
                if line.startswith("[") and line.endswith("]"):
                    section = line[1:-1]
                    continue
                location = f"{path}:{line_number}"
                if section in {"General", "Difficulty"} and ":" in line:
                    name, value = (part.strip() for part in line.split(":", 1))
                    wanted = (section == "General" and name == "Mode") or (
                        section == "Difficulty" and name in {"CircleSize", "SliderMultiplier", "SliderTickRate"}
                    )
                    if not wanted:
                        continue
                    marker = section, name
                    if marker in seen:
                        raise _fail(f"duplicate {name} at {location}")
                    seen.add(marker)
                    if name == "Mode":
                        mode = _integer(value, "Mode", location)
                        if mode not in {0, 1, 2, 3}:
                            raise _fail(f"unsupported Mode {mode} at {location}")
                    elif name == "CircleSize":
                        parsed_size = _decimal(value, "CircleSize", location)
                        if (
                            parsed_size != parsed_size.to_integral_value()
                            or not 1 <= parsed_size <= 10
                        ):
                            circle_size = -1
                        else:
                            circle_size = int(parsed_size)
                    elif name == "SliderMultiplier":
                        slider_multiplier = _decimal(value, name, location)
                        if not Decimal("0.001") <= slider_multiplier <= Decimal(1_000_000):
                            raise _fail(f"invalid {name} at {location}")
                    else:
                        slider_tick_rate = _decimal(value, name, location)
                        if not Decimal("0.001") <= slider_tick_rate <= Decimal(1_000_000):
                            raise _fail(f"invalid {name} at {location}")
                elif section == "TimingPoints":
                    fields = line.split(",")
                    if not 2 <= len(fields) <= 8:
                        raise _fail(f"invalid timing point at {location}")
                    time = _decimal(fields[0], "timing point time", location)
                    beat_length = _decimal(fields[1], "timing point beat length", location)
                    if (
                        not -_MAX_TIMESTAMP_MS <= time <= _MAX_TIMESTAMP_MS
                        or not Decimal("0.001") <= abs(beat_length) <= Decimal(1_000_000)
                    ):
                        raise _fail(f"invalid timing point at {location}")
                    if len(fields) >= 3:
                        meter = _integer(fields[2], "timing point meter", location)
                        if meter <= 0:
                            raise _fail(f"invalid timing point meter at {location}")
                    if len(fields) >= 4 and _integer(fields[3], "timing point sample set", location) < 0:
                        raise _fail(f"invalid timing point sample set at {location}")
                    if len(fields) >= 5 and _integer(fields[4], "timing point sample index", location) < 0:
                        raise _fail(f"invalid timing point sample index at {location}")
                    if len(fields) >= 6:
                        volume = _integer(fields[5], "timing point volume", location)
                        if not 0 <= volume <= 100:
                            raise _fail(f"invalid timing point volume at {location}")
                    if len(fields) >= 7:
                        inherited_flag = _integer(fields[6], "timing point uninherited flag", location)
                        if inherited_flag not in {0, 1}:
                            raise _fail(f"invalid timing point uninherited flag at {location}")
                        uninherited = inherited_flag == 1
                    else:
                        uninherited = beat_length > 0
                    if len(fields) >= 8 and _integer(fields[7], "timing point effects", location) < 0:
                        raise _fail(f"invalid timing point effects at {location}")
                    if (uninherited and beat_length <= 0) or (not uninherited and beat_length >= 0):
                        raise _fail(f"invalid timing point beat length at {location}")
                    if previous_timing is not None and time < previous_timing:
                        raise _fail(f"timing points are not ordered at {location}")
                    previous_timing = time
                    timings.append(_TimingPoint(time, beat_length, uninherited))
                elif section == "HitObjects":
                    if len(object_lines) >= _MAX_OBJECTS:
                        raise _fail(f"too many hit objects at {location}")
                    object_lines.append((line, location))
    except (OSError, UnicodeError) as exc:
        raise _fail(f"cannot read {path}: {exc}") from exc
    if mode is None:
        raise _fail(f"missing Mode in {path}")
    objects = [_parse_object(line, mode, location) for line, location in object_lines]
    has_slider = any(obj.kind == "slider" for obj in objects)
    if has_slider and slider_multiplier is None:
        raise _fail(f"missing SliderMultiplier in {path}")
    if has_slider and slider_tick_rate is None:
        raise _fail(f"missing SliderTickRate in {path}")

    actions: list[Action] = []
    if mode == 3:
        if circle_size is None or not 1 <= circle_size <= 10:
            raise _fail(f"CircleSize must be an integer from 1 to 10 in {path}")
        controls = tuple(f"C{index}" for index in range(circle_size))
        columns: list[list[_Object]] = [[] for _ in controls]
        for obj in objects:
            columns[min(obj.x * circle_size // 512, circle_size - 1)].append(obj)
        for column, notes in enumerate(columns):
            notes.sort(key=lambda obj: obj.start)
            for index, obj in enumerate(notes):
                next_start = notes[index + 1].start if index + 1 < len(notes) else None
                natural_end = obj.end if obj.kind == "hold" else obj.start + tap_duration_ms
                assert natural_end is not None
                if next_start == obj.start:
                    raise _fail(f"multiple notes start simultaneously in column {column + 1} at {obj.start} ms")
                if obj.kind == "hold" and next_start is not None and next_start < natural_end:
                    raise _fail(f"hold overlaps another note in column {column + 1} at {next_start} ms")
                end = min(natural_end, next_start) if obj.kind != "hold" and next_start is not None else natural_end
                _append(actions, [KeyAction(obj.start, controls[column], True), KeyAction(end, controls[column], False)], str(path))
        return PlayPlan(mode, controls, _sort_actions(actions))

    if mode == 1:
        controls = ("DON_LEFT", "DON_RIGHT", "KAT_LEFT", "KAT_RIGHT")
        don_index = kat_index = 0
        for obj in objects:
            if obj.kind == "circle":
                is_kat = bool(obj.hitsound & (2 | 8))
                if is_kat:
                    control = controls[2 + kat_index % 2]
                    kat_index += 1
                else:
                    control = controls[don_index % 2]
                    don_index += 1
                _append(actions, [KeyAction(obj.start, control, True), KeyAction(obj.start + tap_duration_ms, control, False)], str(path))
            else:
                if obj.kind == "slider":
                    assert slider_multiplier is not None and slider_tick_rate is not None
                    duration = _slider_duration(obj, timings, slider_multiplier, str(path))
                    end = obj.start + duration
                    beat, _ = _timing_at(timings, obj.start, str(path))
                    cadence = beat / slider_tick_rate
                else:
                    assert obj.end is not None
                    end = obj.end
                    beat, _ = _timing_at(timings, obj.start, str(path))
                    cadence = beat / Decimal(4)
                hit_times = _sample_times(obj.start, end, cadence, str(path))[:-1]
                roll: list[Action] = []
                for index, timestamp in enumerate(hit_times):
                    control = controls[don_index % 2]
                    don_index += 1
                    next_same_control = hit_times[index + 2] if index + 2 < len(hit_times) else end
                    release = min(timestamp + tap_duration_ms, next_same_control, end)
                    roll.extend((KeyAction(timestamp, control, True), KeyAction(release, control, False)))
                _append(actions, roll, str(path))
        return PlayPlan(mode, controls, _sort_actions(actions))

    if mode == 0:
        controls = ("PRIMARY",)
        for obj in objects:
            if obj.kind == "circle":
                _append(actions, [PointerMove(obj.start, float(obj.x), float(obj.y)), KeyAction(obj.start, "PRIMARY", True), KeyAction(obj.start + tap_duration_ms, "PRIMARY", False)], str(path))
            elif obj.kind == "slider":
                assert slider_multiplier is not None and slider_tick_rate is not None
                duration = _slider_duration(obj, timings, slider_multiplier, str(path))
                end = obj.start + duration
                generated: list[Action] = list(_slider_moves(obj, end, Decimal(16), None, str(path)))
                generated.extend((KeyAction(obj.start, "PRIMARY", True), KeyAction(end, "PRIMARY", False)))
                _append(actions, generated, str(path))
            else:
                assert obj.end is not None
                times = _sample_times(obj.start, obj.end, Decimal(16), str(path))
                radius = 80.0
                generated = [
                    PointerMove(
                        timestamp,
                        256.0 + radius * math.cos(2 * math.pi * 6 * (timestamp - obj.start) / 1000),
                        192.0 + radius * math.sin(2 * math.pi * 6 * (timestamp - obj.start) / 1000),
                    )
                    for timestamp in times
                ]
                generated.extend((KeyAction(obj.start, "PRIMARY", True), KeyAction(obj.end, "PRIMARY", False)))
                _append(actions, generated, str(path))
        return PlayPlan(mode, controls, _sort_actions(actions))

    controls: tuple[str, ...] = ()
    for obj in objects:
        if obj.kind == "circle":
            _append(actions, [PointerMove(obj.start, float(obj.x), 384.0)], str(path))
        elif obj.kind == "slider":
            assert slider_multiplier is not None and slider_tick_rate is not None
            duration = _slider_duration(obj, timings, slider_multiplier, str(path))
            _append(actions, list(_slider_moves(obj, obj.start + duration, Decimal(16), 384.0, str(path))), str(path))
        else:
            continue
    return PlayPlan(mode, controls, _sort_actions(actions))
