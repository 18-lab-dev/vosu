from __future__ import annotations

import base64
import hashlib
import json
import math
import os
import random
import secrets
import socket
import ssl
import struct
import tempfile
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, BinaryIO, Callable, TypeVar
from typing import Protocol
from urllib.parse import urlsplit

MAX_MESSAGE_BYTES = 1024 * 1024
_WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


class ValidationError(Exception):
    pass


def scan_maps(songs: Path, min_ln_ratio: float, limit: int) -> list[dict[str, Any]]:
    if limit < 0:
        raise ValidationError("limit must be non-negative")
    found: list[dict[str, Any]] = []
    for path in sorted(songs.rglob("*.osu"), key=lambda p: str(p).casefold()):
        parsed = parse_osu(path)
        if parsed is None:
            continue
        long_notes, normal_notes = parsed
        ratio = math.inf if long_notes and not normal_notes else (
            long_notes / normal_notes if normal_notes else 0.0
        )
        if ratio < min_ln_ratio:
            continue
        found.append({
            "path": str(path),
            "longNotes": long_notes,
            "normalNotes": normal_notes,
            "lnNormalRatio": ratio,
        })
    found.sort(key=lambda item: (-item["lnNormalRatio"], -item["longNotes"], item["path"].casefold(), item["path"]))
    return found[:limit]


def parse_osu(path: Path) -> tuple[int, int] | None:
    mode: int | None = None
    section = ""
    long_notes = normal_notes = 0
    try:
        with path.open("r", encoding="utf-8-sig", errors="replace", newline=None) as stream:
            for raw_line in stream:
                line = raw_line.strip()
                if not line or line.startswith("//"):
                    continue
                if line.startswith("[") and line.endswith("]"):
                    section = line[1:-1]
                    continue
                if section == "General" and line.startswith("Mode:"):
                    try:
                        mode = int(line.split(":", 1)[1].strip())
                    except ValueError:
                        raise ValidationError(f"invalid Mode in {path}") from None
                elif section == "HitObjects":
                    fields = line.split(",")
                    if len(fields) < 4:
                        raise ValidationError(f"invalid hit object in {path}")
                    try:
                        object_type = int(fields[3])
                    except ValueError:
                        raise ValidationError(f"invalid hit object type in {path}") from None
                    if object_type & 128:
                        long_notes += 1
                    if object_type & 1:
                        normal_notes += 1
    except OSError as exc:
        raise ValidationError(f"cannot read {path}: {exc}") from exc
    return (long_notes, normal_notes) if mode == 3 else None


@dataclass(frozen=True, order=True)
class ManiaEvent:
    timestamp_ms: int
    column: int
    pressed: bool


@dataclass(frozen=True)
class ManiaReplay:
    key_count: int
    events: tuple[ManiaEvent, ...]


def parse_mania_replay(path: Path, tap_duration_ms: int = 40) -> ManiaReplay:
    if isinstance(tap_duration_ms, bool) or not isinstance(tap_duration_ms, int) or tap_duration_ms <= 0:
        raise ValidationError("tap duration must be a positive integer number of milliseconds")
    mode: int | None = None
    key_count: int | None = None
    section = ""
    objects: list[tuple[int, int, int, bool]] = []
    try:
        with path.open("r", encoding="utf-8-sig", errors="replace", newline=None) as stream:
            for line_number, raw_line in enumerate(stream, 1):
                line = raw_line.strip()
                if not line or line.startswith("//"):
                    continue
                if line.startswith("[") and line.endswith("]"):
                    section = line[1:-1]
                    continue
                if section == "General" and line.startswith("Mode:"):
                    try:
                        mode = int(line.split(":", 1)[1].strip())
                    except ValueError:
                        raise ValidationError(f"invalid Mode in {path}") from None
                elif section == "Difficulty" and line.startswith("CircleSize:"):
                    value = line.split(":", 1)[1].strip()
                    try:
                        key_count = int(value)
                    except ValueError:
                        raise ValidationError(f"CircleSize must be an integer in {path}") from None
                    if key_count <= 0:
                        raise ValidationError(f"CircleSize must be positive in {path}")
                elif section == "HitObjects":
                    fields = line.split(",")
                    if len(fields) < 5:
                        raise ValidationError(f"invalid hit object at {path}:{line_number}")
                    try:
                        x = int(fields[0])
                        start_ms = int(fields[2])
                        object_type = int(fields[3])
                    except ValueError:
                        raise ValidationError(f"invalid hit object at {path}:{line_number}") from None
                    if not 0 <= x <= 512 or start_ms < 0:
                        raise ValidationError(f"invalid hit object at {path}:{line_number}")
                    if object_type & 128:
                        if len(fields) < 6:
                            raise ValidationError(f"hold has no end time at {path}:{line_number}")
                        try:
                            end_ms = int(fields[5].split(":", 1)[0])
                        except ValueError:
                            raise ValidationError(f"invalid hold end time at {path}:{line_number}") from None
                        if end_ms <= start_ms:
                            raise ValidationError(f"hold must end after it starts at {path}:{line_number}")
                        objects.append((x, start_ms, end_ms, True))
                    elif object_type & 1:
                        objects.append((x, start_ms, start_ms + tap_duration_ms, False))
                    else:
                        raise ValidationError(
                            f"unsupported hit object type {object_type} at {path}:{line_number}"
                        )
    except OSError as exc:
        raise ValidationError(f"cannot read {path}: {exc}") from exc
    if mode != 3:
        raise ValidationError(f"{path} is not an osu!mania map (Mode must be 3)")
    if key_count is None:
        raise ValidationError(f"missing CircleSize in {path}")
    if key_count > 10:
        raise ValidationError(f"CircleSize above 10 is not supported in {path}")

    columns: list[list[tuple[int, int, bool]]] = [[] for _ in range(key_count)]
    for x, start_ms, end_ms, is_hold in objects:
        columns[min(x * key_count // 512, key_count - 1)].append((start_ms, end_ms, is_hold))

    events: list[ManiaEvent] = []
    for column, notes in enumerate(columns):
        notes.sort()
        for index, (start_ms, natural_end_ms, is_hold) in enumerate(notes):
            next_start_ms = notes[index + 1][0] if index + 1 < len(notes) else None
            if next_start_ms == start_ms:
                raise ValidationError(
                    f"multiple notes start simultaneously in column {column + 1} at {start_ms} ms"
                )
            if is_hold and next_start_ms is not None and next_start_ms < natural_end_ms:
                raise ValidationError(
                    f"hold overlaps another note in column {column + 1} at {next_start_ms} ms"
                )
            end_ms = (
                min(natural_end_ms, next_start_ms)
                if not is_hold and next_start_ms is not None
                else natural_end_ms
            )
            events.append(ManiaEvent(start_ms, column, True))
            events.append(ManiaEvent(end_ms, column, False))
    events.sort()
    return ManiaReplay(key_count, tuple(events))


DEFAULT_4K_KEYS = ("D", "F", "J", "K")
_LINUX_KEY_CODES = {
    **{chr(ord("A") + index): code for index, code in enumerate(
        (30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44)
    )},
    **{key: code for key, code in zip("1234567890", range(2, 12))},
    "MINUS": 12,
    "ESCAPE": 1,
    "SPACE": 57,
    "ENTER": 28,
}
_WINDOWS_SCAN_CODES = {
    **{key: code for key, code in zip(
        "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
        (0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26,
         0x32, 0x31, 0x18, 0x19, 0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C)
    )},
    **{key: code for key, code in zip("1234567890", range(0x02, 0x0C))},
    "MINUS": 0x0C,
    "ESCAPE": 0x01,
    "SPACE": 0x39,
    "ENTER": 0x1C,
}


def normalize_key_name(value: str, option: str = "key") -> str:
    key = value.strip().upper()
    if key not in _LINUX_KEY_CODES:
        raise ValidationError(f"unsupported {option} name: {key or value!r}")
    return key


def parse_key_mapping(value: str | None, key_count: int) -> tuple[str, ...]:
    if value is None:
        if key_count != 4:
            raise ValidationError(f"--keys is required for {key_count}K maps")
        return DEFAULT_4K_KEYS
    keys = tuple(part.strip().upper() for part in value.split(","))
    if len(keys) != key_count or any(not key for key in keys):
        raise ValidationError(f"key mapping must contain exactly {key_count} comma-separated keys")
    unknown = [key for key in keys if key not in _LINUX_KEY_CODES]
    if unknown:
        raise ValidationError(f"unsupported key name: {unknown[0]}")
    if len(set(keys)) != len(keys):
        raise ValidationError("key mapping must not contain duplicate keys")
    return keys


class KeyboardBackend(Protocol):
    def emit(self, key: str, pressed: bool) -> None: ...
    def close(self) -> None: ...


class DryRunBackend:
    def __init__(self, output: Callable[[str], None] = print):
        self.output = output

    def emit(self, key: str, pressed: bool) -> None:
        pass

    def close(self) -> None:
        pass


class WindowsSendInputBackend:
    def __init__(self) -> None:
        if os.name != "nt":
            raise ValidationError("windows backend is only available on Windows")
        import ctypes
        from ctypes import wintypes

        ulong_ptr = ctypes.c_size_t

        class MOUSEINPUT(ctypes.Structure):
            _fields_ = [
                ("dx", wintypes.LONG), ("dy", wintypes.LONG),
                ("mouseData", wintypes.DWORD), ("dwFlags", wintypes.DWORD),
                ("time", wintypes.DWORD), ("dwExtraInfo", ulong_ptr),
            ]

        class KEYBDINPUT(ctypes.Structure):
            _fields_ = [
                ("wVk", wintypes.WORD), ("wScan", wintypes.WORD),
                ("dwFlags", wintypes.DWORD), ("time", wintypes.DWORD),
                ("dwExtraInfo", ulong_ptr),
            ]

        class HARDWAREINPUT(ctypes.Structure):
            _fields_ = [
                ("uMsg", wintypes.DWORD),
                ("wParamL", wintypes.WORD), ("wParamH", wintypes.WORD),
            ]

        class INPUTUNION(ctypes.Union):
            _fields_ = [("mi", MOUSEINPUT), ("ki", KEYBDINPUT), ("hi", HARDWAREINPUT)]

        class INPUT(ctypes.Structure):
            _anonymous_ = ("value",)
            _fields_ = [("type", wintypes.DWORD), ("value", INPUTUNION)]

        self._ctypes = ctypes
        self._keybd_input = KEYBDINPUT
        self._input = INPUT
        self._send_input = ctypes.WinDLL("user32", use_last_error=True).SendInput
        self._send_input.argtypes = (wintypes.UINT, ctypes.POINTER(INPUT), ctypes.c_int)
        self._send_input.restype = wintypes.UINT

    def emit(self, key: str, pressed: bool) -> None:
        flags = 0x0008 | (0 if pressed else 0x0002)
        event = self._input(
            type=1,
            ki=self._keybd_input(
                wVk=0, wScan=_WINDOWS_SCAN_CODES[key], dwFlags=flags,
                time=0, dwExtraInfo=0,
            ),
        )
        if self._send_input(1, self._ctypes.byref(event), self._ctypes.sizeof(event)) != 1:
            raise self._ctypes.WinError(self._ctypes.get_last_error())

    def close(self) -> None:
        pass


def _ioctl(write: bool, type_: int, number: int, size: int) -> int:
    direction = 1 << 30 if write else 0
    return direction | (size << 16) | (type_ << 8) | number


class UInputBackend:
    def __init__(self, keys: tuple[str, ...], path: str = "/dev/uinput") -> None:
        import fcntl
        machine = os.uname().machine.casefold()
        if machine not in {"x86_64", "amd64", "aarch64", "arm64"}:
            raise ValidationError(f"uinput backend does not support Linux architecture {machine}")

        try:
            self._fd = os.open(path, os.O_WRONLY | os.O_NONBLOCK)
        except PermissionError as exc:
            raise ValidationError(
                f"permission denied opening {path}; grant the current user write access to /dev/uinput"
            ) from exc
        except OSError as exc:
            raise ValidationError(f"cannot open {path}: {exc}") from exc
        try:
            fcntl.ioctl(self._fd, _ioctl(True, ord("U"), 100, 4), 1)
            for key in dict.fromkeys(keys):
                fcntl.ioctl(self._fd, _ioctl(True, ord("U"), 101, 4), _LINUX_KEY_CODES[key])
            device_name = f"vosu input validation {os.getpid()}"
            descriptor = struct.pack(
                "@80sHHHHI256i",
                device_name.encode(), 0x03, 0, 0, 1, 0, *([0] * 256),
            )
            os.write(self._fd, descriptor)
            fcntl.ioctl(self._fd, _ioctl(False, ord("U"), 1, 0))
            deadline = time.monotonic() + 1.0
            while not any(
                name_path.read_text(encoding="utf-8").strip() == device_name
                for name_path in Path("/sys/class/input").glob("input*/name")
            ):
                if time.monotonic() >= deadline:
                    raise ValidationError("uinput keyboard did not become observable")
                time.sleep(0.01)
            time.sleep(0.05)
        except BaseException:
            os.close(self._fd)
            raise

    def emit(self, key: str, pressed: bool) -> None:
        event = struct.pack("llHHi", 0, 0, 1, _LINUX_KEY_CODES[key], int(pressed))
        sync = struct.pack("llHHi", 0, 0, 0, 0, 0)
        os.write(self._fd, event + sync)

    def close(self) -> None:
        import fcntl

        if self._fd >= 0:
            try:
                time.sleep(0.05)
                fcntl.ioctl(self._fd, _ioctl(False, ord("U"), 2, 0))
            finally:
                os.close(self._fd)
                self._fd = -1


def create_keyboard_backend(name: str, keys: tuple[str, ...]) -> KeyboardBackend:
    if name == "auto":
        name = "windows" if os.name == "nt" else "uinput"
    if name == "windows":
        return WindowsSendInputBackend()
    if name == "uinput":
        if os.name == "nt":
            raise ValidationError("uinput backend is only available on Linux")
        return UInputBackend(keys)
    if name == "dry-run":
        return DryRunBackend()
    raise ValidationError(f"unknown backend: {name}")

# Stable sequences follow https://osu.ppy.sh/wiki/en/Client/Keyboard_shortcuts.
# Lazer's extra multiplayer step follows ButtonSystem.cs in ppy/osu: P opens the
# top-level/play states, M opens the multiplayer state, then M activates Lounge.
_UI_NAVIGATION_PATHS = {
    ("lazer", "song-select"): ("P", "P", "P"),
    ("lazer", "multiplayer"): ("P", "P", "M", "M"),
    ("stable", "song-select"): ("P", "P", "P"),
    ("stable", "multiplayer"): ("P", "P", "M"),
}


def _search_key_name(character: str) -> str:
    if character.isascii() and character.isalpha():
        return character.upper()
    if character.isascii() and character.isdigit():
        return character
    if character == " ":
        return "SPACE"
    if character == "-":
        return "MINUS"
    raise ValidationError(f"unsupported search character: {character!r}")


def navigation_key_names(client: str, target: str, search: str | None) -> tuple[str, ...]:
    try:
        path = _UI_NAVIGATION_PATHS[(client, target)]
    except KeyError as exc:
        raise ValidationError(f"unsupported {client} navigation target: {target}") from exc
    keys = list(path)
    if search is not None:
        if target != "song-select":
            raise ValidationError("search requires song-select navigation")
        keys.extend(_search_key_name(character) for character in search)
    return tuple(dict.fromkeys(keys))


def navigate_ui(
    client: str,
    target: str,
    backend: KeyboardBackend,
    search: str | None = None,
    key_delay: float = 0.1,
    typing_delay: float = 0.001,
    transition_delay: float = 1.0,
    *,
    sleep: Callable[[float], None] = time.sleep,
) -> None:
    pressed: set[str] = set()
    try:
        for name, value in (
            ("key delay", key_delay),
            ("typing delay", typing_delay),
            ("transition delay", transition_delay),
        ):
            if not math.isfinite(value) or value < 0:
                raise ValidationError(f"{name} must be a finite non-negative number")
        if key_delay < 0.04:
            raise ValidationError("key delay must be at least 0.04 seconds")
        if typing_delay < 0.001:
            raise ValidationError("typing delay must be at least 0.001 seconds")
        navigation_key_names(client, target, search)
        path = _UI_NAVIGATION_PATHS[(client, target)]

        def emit(key: str, is_pressed: bool) -> None:
            backend.emit(key, is_pressed)
            if is_pressed:
                pressed.add(key)
            else:
                pressed.discard(key)

        for index, key in enumerate(path):
            emit(key, True)
            sleep(0.04)
            emit(key, False)
            if index + 1 < len(path):
                sleep(key_delay - 0.04)

        if search is not None:
            sleep(transition_delay)
            search_keys = tuple(_search_key_name(character) for character in search)
            for index, key in enumerate(search_keys):
                emit(key, True)
                sleep(0.001)
                emit(key, False)
                if index + 1 < len(search_keys):
                    remaining_delay = typing_delay - 0.001
                    if remaining_delay:
                        sleep(remaining_delay)
    finally:
        active_error = sys.exc_info()[1]
        cleanup_error: BaseException | None = None
        for key in sorted(pressed):
            try:
                backend.emit(key, False)
            except BaseException as exc:
                if cleanup_error is None:
                    cleanup_error = exc
        try:
            backend.close()
        except BaseException as exc:
            if cleanup_error is None:
                cleanup_error = exc
        if cleanup_error is not None and active_error is None:
            raise cleanup_error


def autoplay(
    replay: ManiaReplay,
    keys: tuple[str, ...],
    backend: KeyboardBackend,
    start_delay: float,
) -> None:
    pressed: set[str] = set()
    try:
        if not math.isfinite(start_delay) or start_delay < 0:
            raise ValidationError("start delay must be a finite non-negative number")
        if len(keys) != replay.key_count:
            raise ValidationError(f"key mapping must contain exactly {replay.key_count} keys")
        if isinstance(backend, DryRunBackend):
            for event in replay.events:
                action = "down" if event.pressed else "up"
                backend.output(f"{event.timestamp_ms:>7} ms  C{event.column + 1} {keys[event.column]} {action}")
            return

        origin_ns = time.perf_counter_ns() + round(start_delay * 1_000_000_000)
        for event in replay.events:
            target_ns = origin_ns + event.timestamp_ms * 1_000_000
            while True:
                remaining_ns = target_ns - time.perf_counter_ns()
                if remaining_ns <= 0:
                    break
                if remaining_ns > 2_000_000:
                    time.sleep((remaining_ns - 1_000_000) / 1_000_000_000)
            key = keys[event.column]
            if event.pressed:
                pressed.add(key)
                backend.emit(key, True)
            else:
                backend.emit(key, False)
                pressed.discard(key)
    finally:
        active_error = sys.exc_info()[1]
        cleanup_error: BaseException | None = None
        for key in sorted(pressed):
            try:
                backend.emit(key, False)
            except BaseException as exc:
                if cleanup_error is None:
                    cleanup_error = exc
        try:
            backend.close()
        except BaseException as exc:
            if cleanup_error is None:
                cleanup_error = exc
        if cleanup_error is not None and active_error is None:
            raise cleanup_error


def parse_live_state(payload: Any, received_at: float) -> tuple[float, str, float, bool]:
    try:
        state_number = payload["state"]["number"]
        live_ms = payload["beatmap"]["time"]["live"]
        paused = payload["game"]["paused"]
    except (KeyError, TypeError) as exc:
        raise ValidationError(
            "live state is missing state.number, beatmap.time.live, or game.paused"
        ) from exc
    state_names = {5: "selectPlay", 2: "play"}
    if isinstance(state_number, bool) or not isinstance(state_number, int):
        raise ValidationError("live state state.number must be an integer")
    if isinstance(live_ms, bool) or not isinstance(live_ms, (int, float)) or not math.isfinite(live_ms):
        raise ValidationError("live state beatmap.time.live must be a finite number")
    if not isinstance(paused, bool):
        raise ValidationError("live state game.paused must be a boolean")
    return received_at, state_names.get(state_number, f"status {state_number}"), float(live_ms), paused


def jittered_events(
    events: tuple[ManiaEvent, ...],
    jitter_ms: float,
    seed: int,
) -> tuple[tuple[float, ManiaEvent], ...]:
    if not math.isfinite(jitter_ms) or jitter_ms < 0:
        raise ValidationError("jitter must be a finite non-negative number")
    generator = random.Random(seed)
    active_offsets: dict[int, float] = {}
    result: list[tuple[float, ManiaEvent]] = []
    previous = -math.inf
    for event in events:
        if event.pressed:
            offset = generator.uniform(-jitter_ms, jitter_ms) if jitter_ms else 0.0
            active_offsets[event.column] = offset
        else:
            offset = active_offsets.pop(event.column, 0.0)
        timestamp = max(previous, event.timestamp_ms + offset)
        result.append((timestamp, event))
        previous = timestamp
    return tuple(result)


LiveStateReader = Callable[[str, float, Callable[[], float]], tuple[float, str, float, bool]]


_TimelineEvent = TypeVar("_TimelineEvent")


def run_live_timeline(
    scheduled: tuple[tuple[float, _TimelineEvent], ...],
    emit_action: Callable[[_TimelineEvent], None],
    base_url: str,
    start_action: Callable[[], None] | None = None,
    start_timeout: float = 10.0,
    *,
    state_reader: LiveStateReader | None = None,
    monotonic: Callable[[], float] = time.monotonic,
    request_timeout: float = 0.25,
    stall_timeout: float = 5.0,
) -> None:
    if not math.isfinite(start_timeout) or start_timeout <= 0:
        raise ValidationError("start timeout must be a finite positive number")
    if not math.isfinite(request_timeout) or request_timeout <= 0:
        raise ValidationError("state timeout must be a finite positive number")
    if not math.isfinite(stall_timeout) or stall_timeout <= 0:
        raise ValidationError("stall timeout must be a finite positive number")

    state_socket: WebSocket | None = None
    try:
        reader = state_reader
        if reader is None:
            state_socket = websocket_connect(base_url, request_timeout, "/websocket/v2")
            live_socket = state_socket

            def read_state(
                unused_base_url: str,
                unused_timeout: float,
                clock: Callable[[], float],
            ) -> tuple[float, str, float, bool]:
                live_socket.sock.settimeout(unused_timeout)
                message = live_socket.receive_message()
                if message is None:
                    raise ValidationError("live state websocket closed")
                try:
                    payload = json.loads(message)
                except json.JSONDecodeError as exc:
                    raise ValidationError("live state websocket sent invalid JSON") from exc
                return parse_live_state(payload, clock())

            reader = read_state

        deadline = monotonic() + start_timeout

        def read_start_state() -> tuple[float, str, float, bool]:
            while True:
                remaining = deadline - monotonic()
                if remaining <= 0:
                    raise ValidationError(
                        "timed out waiting for a fresh selectPlay -> play timeline reset"
                    )
                try:
                    state = reader(base_url, min(request_timeout, remaining), monotonic)
                except TimeoutError:
                    continue
                if monotonic() > deadline:
                    raise ValidationError(
                        "timed out waiting for a fresh selectPlay -> play timeline reset"
                    )
                return state

        _, state_name, live_ms, paused = read_start_state()
        saw_select_play = state_name == "selectPlay"

        if start_action is not None:
            start_action()

        while True:
            received_at, state_name, live_ms, paused = read_start_state()
            if state_name == "selectPlay":
                saw_select_play = True
            elif state_name == "play" and saw_select_play and live_ms <= 1000.0:
                break
            elif state_name not in ("selectPlay", "play"):
                raise ValidationError(f"game left start flow in state {state_name!r}")
            # lazer can remain in play with stale preview time before resetting to gameplay time.

        previous_live_ms = live_ms
        progress_live_ms = live_ms
        last_progress_at = received_at
        anchor_received_at = received_at
        anchor_live_ms = live_ms
        startup_live_ms = live_ms
        extrapolation_ready = False
        for target_ms, event in scheduled:
            while True:
                now = monotonic()
                estimated_live_ms = anchor_live_ms
                if not paused and extrapolation_ready:
                    estimated_live_ms += (now - anchor_received_at) * 1000.0
                if not paused and estimated_live_ms >= target_ms - 1e-6:
                    break

                stall_remaining = last_progress_at + stall_timeout - now
                if stall_remaining <= 0:
                    raise ValidationError(
                        "gameplay live time stalled "
                        f"(state={state_name!r}, live_ms={live_ms}, paused={paused})"
                    )
                event_remaining = math.inf
                if not paused:
                    event_remaining = max(
                        0.0, (target_ms - estimated_live_ms) / 1000.0
                    )
                timeout = min(request_timeout, event_remaining, stall_remaining)
                try:
                    (
                        sample_received_at,
                        sample_state_name,
                        sample_live_ms,
                        sample_paused,
                    ) = reader(base_url, timeout, monotonic)
                except TimeoutError:
                    continue
                if not extrapolation_ready:
                    anchor_live_ms = sample_live_ms
                    anchor_received_at = sample_received_at
                    extrapolation_ready = (
                        not sample_paused
                        and sample_live_ms > startup_live_ms + 1.0
                    )
                else:
                    estimated_at_sample = anchor_live_ms
                    if not paused:
                        estimated_at_sample += (
                            sample_received_at - anchor_received_at
                        ) * 1000.0
                    if sample_paused or paused:
                        anchor_live_ms = sample_live_ms
                    else:
                        anchor_live_ms = max(sample_live_ms, estimated_at_sample)
                    anchor_received_at = sample_received_at
                received_at = sample_received_at
                state_name = sample_state_name
                live_ms = sample_live_ms
                paused = sample_paused
                if state_name != "play":
                    raise ValidationError(f"game left play state for {state_name!r}")
                if live_ms + 100.0 < previous_live_ms:
                    raise ValidationError("gameplay timeline reset before autoplay completed")
                previous_live_ms = live_ms
                if live_ms > progress_live_ms + 1.0:
                    progress_live_ms = live_ms
                    last_progress_at = received_at
            emit_action(event)
    finally:
        if state_socket is not None:
            state_socket.close()


def live_autoplay(
    replay: ManiaReplay,
    keys: tuple[str, ...],
    backend: KeyboardBackend,
    base_url: str,
    start_key: str | None = "ENTER",
    start_timeout: float = 10.0,
    jitter_ms: float = 0.0,
    seed: int = 0,
    *,
    state_reader: LiveStateReader | None = None,
    monotonic: Callable[[], float] = time.monotonic,
    sleep: Callable[[float], None] = time.sleep,
    request_timeout: float = 0.25,
    stall_timeout: float = 5.0,
) -> None:
    pressed: set[str] = set()
    try:
        if len(keys) != replay.key_count:
            raise ValidationError(f"key mapping must contain exactly {replay.key_count} keys")
        if start_key is not None:
            start_key = normalize_key_name(start_key, "start key")
        scheduled = jittered_events(replay.events, jitter_ms, seed)

        def start() -> None:
            assert start_key is not None
            pressed.add(start_key)
            backend.emit(start_key, True)
            sleep(0.04)
            backend.emit(start_key, False)
            pressed.discard(start_key)

        def emit(event: ManiaEvent) -> None:
            key = keys[event.column]
            if event.pressed:
                pressed.add(key)
                backend.emit(key, True)
            else:
                backend.emit(key, False)
                pressed.discard(key)

        run_live_timeline(
            scheduled,
            emit,
            base_url,
            start if start_key is not None else None,
            start_timeout,
            state_reader=state_reader,
            monotonic=monotonic,
            request_timeout=request_timeout,
            stall_timeout=stall_timeout,
        )
    finally:
        active_error = sys.exc_info()[1]
        cleanup_error: BaseException | None = None
        for key in sorted(pressed):
            try:
                backend.emit(key, False)
            except BaseException as exc:
                if cleanup_error is None:
                    cleanup_error = exc
        try:
            backend.close()
        except BaseException as exc:
            if cleanup_error is None:
                cleanup_error = exc
        if cleanup_error is not None and active_error is None:
            raise cleanup_error


def atomic_json_write(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", suffix=".tmp", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as stream:
            json.dump(value, stream, ensure_ascii=False, separators=(",", ":"), sort_keys=True, allow_nan=False)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


def _read_exact(stream: BinaryIO, length: int) -> bytes:
    chunks = bytearray()
    while len(chunks) < length:
        chunk = stream.read(length - len(chunks))
        if not chunk:
            raise ValidationError("unexpected end of websocket stream")
        chunks.extend(chunk)
    return bytes(chunks)


@dataclass(frozen=True)
class Frame:
    fin: bool
    opcode: int
    payload: bytes


def _decode_frame_prefix(first: int, second: int) -> tuple[bool, int, int]:
    if first & 0x70:
        raise ValidationError("websocket RSV bits are not supported")
    if second & 0x80:
        raise ValidationError("server websocket frames must not be masked")
    return bool(first & 0x80), first & 0x0F, second & 0x7F


def _decode_frame_header(
    first: int,
    second: int,
    encoded_length: bytes,
    max_payload: int,
) -> tuple[bool, int, int]:
    fin, opcode, length = _decode_frame_prefix(first, second)
    if length == 126:
        length = struct.unpack("!H", encoded_length)[0]
        if length < 126:
            raise ValidationError("non-minimal websocket payload length")
    elif length == 127:
        if encoded_length[0] & 0x80:
            raise ValidationError("invalid websocket payload length")
        length = struct.unpack("!Q", encoded_length)[0]
        if length < 65536:
            raise ValidationError("non-minimal websocket payload length")
    if opcode >= 8 and (not fin or length > 125):
        raise ValidationError("invalid websocket control frame")
    if opcode not in (0, 1, 2, 8, 9, 10):
        raise ValidationError(f"unsupported websocket opcode {opcode}")
    if length > max_payload:
        raise ValidationError("websocket frame exceeds 1 MiB bound")
    return fin, opcode, length


def _decode_buffered_frame(
    buffer: bytearray,
    max_payload: int = MAX_MESSAGE_BYTES,
) -> tuple[Frame, int] | None:
    if len(buffer) < 2:
        return None
    first, second = buffer[0], buffer[1]
    _, _, length_marker = _decode_frame_prefix(first, second)
    extended_length_size = 2 if length_marker == 126 else 8 if length_marker == 127 else 0
    header_size = 2 + extended_length_size
    if len(buffer) < header_size:
        return None
    encoded_length = bytes(buffer[2:header_size])
    fin, opcode, payload_length = _decode_frame_header(
        first,
        second,
        encoded_length,
        max_payload,
    )
    frame_size = header_size + payload_length
    if len(buffer) < frame_size:
        return None
    return Frame(fin, opcode, bytes(buffer[header_size:frame_size])), frame_size


def decode_frame(stream: BinaryIO, max_payload: int = MAX_MESSAGE_BYTES) -> Frame:
    header = _read_exact(stream, 2)
    first, second = header
    _decode_frame_prefix(first, second)
    length_marker = second & 0x7F
    extended_length_size = 2 if length_marker == 126 else 8 if length_marker == 127 else 0
    encoded_length = _read_exact(stream, extended_length_size)
    fin, opcode, length = _decode_frame_header(first, second, encoded_length, max_payload)
    return Frame(fin, opcode, _read_exact(stream, length))


def encode_client_frame(opcode: int, payload: bytes = b"") -> bytes:
    if len(payload) > 125 and opcode >= 8:
        raise ValidationError("control payload is too large")
    first = 0x80 | opcode
    mask = secrets.token_bytes(4)
    length = len(payload)
    if length < 126:
        header = bytes((first, 0x80 | length))
    elif length < 65536:
        header = bytes((first, 0x80 | 126)) + struct.pack("!H", length)
    else:
        header = bytes((first, 0x80 | 127)) + struct.pack("!Q", length)
    masked = bytes(byte ^ mask[index & 3] for index, byte in enumerate(payload))
    return header + mask + masked


class WebSocket:
    def __init__(self, sock: socket.socket):
        self.sock = sock
        self.receive_buffer = bytearray()
        self.fragment_opcode: int | None = None
        self.fragments = bytearray()

    def send(self, opcode: int, payload: bytes = b"") -> None:
        self.sock.sendall(encode_client_frame(opcode, payload))

    def _receive_frame(self) -> Frame:
        while True:
            decoded = _decode_buffered_frame(self.receive_buffer)
            if decoded is not None:
                frame, frame_size = decoded
                del self.receive_buffer[:frame_size]
                return frame
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ValidationError("unexpected end of websocket stream")
            self.receive_buffer.extend(chunk)

    def receive_message(self) -> bytes | None:
        while True:
            frame = self._receive_frame()
            if frame.opcode == 9:
                self.send(10, frame.payload)
                continue
            if frame.opcode == 10:
                continue
            if frame.opcode == 8:
                if len(frame.payload) == 1:
                    raise ValidationError("invalid websocket close payload")
                self.send(8, frame.payload)
                return None
            if frame.opcode == 0:
                if self.fragment_opcode is None:
                    raise ValidationError("unexpected websocket continuation")
                if len(self.fragments) + len(frame.payload) > MAX_MESSAGE_BYTES:
                    raise ValidationError("fragmented websocket message exceeds 1 MiB bound")
                self.fragments.extend(frame.payload)
                if not frame.fin:
                    continue
                opcode, payload = self.fragment_opcode, bytes(self.fragments)
                self.fragment_opcode = None
                self.fragments.clear()
            else:
                if self.fragment_opcode is not None:
                    raise ValidationError("new data frame during fragmented message")
                opcode, payload = frame.opcode, frame.payload
                if len(frame.payload) > MAX_MESSAGE_BYTES:
                    raise ValidationError("websocket message exceeds 1 MiB bound")
                if not frame.fin:
                    self.fragment_opcode = opcode
                    self.fragments.extend(payload)
                    continue
            if opcode != 1:
                raise ValidationError("expected websocket text message")
            try:
                payload.decode("utf-8", errors="strict")
            except UnicodeDecodeError as exc:
                raise ValidationError("invalid UTF-8 websocket message") from exc
            return payload

    def close(self) -> None:
        self.sock.close()


def websocket_connect(
    base_url: str,
    timeout: float,
    endpoint: str = "/websocket/v2/precise",
) -> WebSocket:
    parsed = urlsplit(base_url)
    if parsed.scheme not in ("http", "https", "ws", "wss") or not parsed.hostname:
        raise ValidationError("base URL must use http, https, ws, or wss")
    secure = parsed.scheme in ("https", "wss")
    try:
        port = parsed.port or (443 if secure else 80)
    except ValueError as exc:
        raise ValidationError(f"invalid base URL port: {exc}") from exc
    base_path = parsed.path.rstrip("/")
    path = f"{base_path}{endpoint}" or endpoint
    if parsed.query or parsed.fragment:
        raise ValidationError("base URL must not contain a query or fragment")
    raw = socket.create_connection((parsed.hostname, port), timeout=timeout)
    try:
        sock = ssl.create_default_context().wrap_socket(raw, server_hostname=parsed.hostname) if secure else raw
        sock.settimeout(timeout)
        key = base64.b64encode(secrets.token_bytes(16)).decode("ascii")
        default_port = 443 if secure else 80
        hostname = f"[{parsed.hostname}]" if ":" in parsed.hostname else parsed.hostname
        host = hostname if port == default_port else f"{hostname}:{port}"
        request = (
            f"GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\n"
            f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        ).encode("ascii")
        sock.sendall(request)
        response = bytearray()
        while not response.endswith(b"\r\n\r\n"):
            chunk = sock.recv(1)
            if not chunk:
                raise ValidationError("connection closed during websocket handshake")
            response.extend(chunk)
            if len(response) > 65536:
                raise ValidationError("websocket handshake headers are too large")
        head = bytes(response[:-4])
        try:
            lines = head.decode("ascii").split("\r\n")
        except UnicodeDecodeError as exc:
            raise ValidationError("non-ASCII websocket handshake") from exc
        status = lines[0].split(" ", 2)
        if len(status) < 2 or status[0] != "HTTP/1.1" or status[1] != "101":
            raise ValidationError(f"websocket handshake rejected: {lines[0]}")
        headers: dict[str, list[str]] = {}
        for line in lines[1:]:
            if ":" not in line:
                raise ValidationError("malformed websocket handshake header")
            name, value = line.split(":", 1)
            headers.setdefault(name.strip().lower(), []).append(value.strip())
        tokens = lambda name: {token.strip().lower() for value in headers.get(name, []) for token in value.split(",")}
        expected = base64.b64encode(hashlib.sha1((key + _WS_GUID).encode("ascii")).digest()).decode("ascii")
        if "websocket" not in tokens("upgrade") or "upgrade" not in tokens("connection"):
            raise ValidationError("invalid websocket upgrade headers")
        if headers.get("sec-websocket-accept") != [expected]:
            raise ValidationError("invalid Sec-WebSocket-Accept")
        if "sec-websocket-extensions" in headers or "sec-websocket-protocol" in headers:
            raise ValidationError("server selected an unrequested websocket extension or protocol")
        return WebSocket(sock)
    except BaseException:
        raw.close()
        raise


class CaptureAccounting:
    def __init__(self) -> None:
        self.events: list[dict[str, Any]] = []
        self.states: dict[str, dict[str, Any]] = {}
        self.completed_states: dict[str, dict[str, Any]] = {}
        self.active_mode: int | None = None
        self.segment = 1
        self.receive_times: list[int] = []
        self.failures: list[dict[str, Any]] = []
        self.frames = 0

    @staticmethod
    def _timestamp(value: Any, field: str) -> int | float | None:
        if value is None:
            return None
        if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
            raise ValidationError(f"keypress {field} must be a finite number or null")
        return value

    def _finish_segment(self) -> None:
        if not self.states:
            return
        self.completed_states.update(self.states)
        self.states.clear()
        self.segment += 1

    def _key_for_segment(self, name: str) -> str:
        return name if self.segment == 1 else f"{name}@{self.segment}"

    def process(self, payload: Any, receive_ns: int) -> None:
        if not isinstance(payload, dict):
            raise ValidationError("precise payload must be an object")
        keypresses = payload.get("keypresses")
        if not isinstance(keypresses, dict) or not isinstance(keypresses.get("keys"), list):
            raise ValidationError("precise payload has no keypresses.keys array")
        mode = keypresses.get("mode")
        mode_number = mode.get("number") if isinstance(mode, dict) else None
        if isinstance(mode_number, bool) or not isinstance(mode_number, int):
            raise ValidationError("precise payload has no integer keypresses.mode.number")
        keys = keypresses["keys"]
        self.receive_times.append(receive_ns)
        self.frames += 1
        if self.active_mode is not None and mode_number != self.active_mode:
            self._finish_segment()
        self.active_mode = mode_number
        if not keys:
            self._finish_segment()
            return
        probe_duplicates: dict[str, int] = {}
        for item in keys:
            if not isinstance(item, dict) or not isinstance(item.get("name"), str):
                raise ValidationError("keypress entry must have a string name")
            pressed = item.get("isPressed")
            count = item.get("count")
            if not isinstance(pressed, bool) or isinstance(count, bool) or not isinstance(count, int):
                raise ValidationError("keypress isPressed/count have invalid types")
            name = item["name"]
            occurrence = probe_duplicates.get(name, 0)
            probe_duplicates[name] = occurrence + 1
            base_key = name if occurrence == 0 else f"{name}#{occurrence + 1}"
            prior = self.states.get(self._key_for_segment(base_key))
            if prior is not None and count < prior["count"]:
                self._finish_segment()
                break
        duplicate_counts: dict[str, int] = {}
        for item in keys:
            if not isinstance(item, dict) or not isinstance(item.get("name"), str):
                raise ValidationError("keypress entry must have a string name")
            name = item["name"]
            occurrence = duplicate_counts.get(name, 0)
            duplicate_counts[name] = occurrence + 1
            base_key = name if occurrence == 0 else f"{name}#{occurrence + 1}"
            key = self._key_for_segment(base_key)
            pressed = item.get("isPressed")
            count = item.get("count")
            if not isinstance(pressed, bool) or isinstance(count, bool) or not isinstance(count, int):
                raise ValidationError("keypress isPressed/count have invalid types")
            pressed_at = self._timestamp(item.get("pressedAt"), "pressedAt")
            released_at = self._timestamp(item.get("releasedAt"), "releasedAt")
            current = {"pressed": pressed, "count": count, "pressedAt": pressed_at, "releasedAt": released_at}
            prior = self.states.get(key)
            if prior is None:
                current.update(firstCount=count, observedPresses=0, observedReleases=0, durations=[], pendingPress=None, mode=mode_number, segment=self.segment)
                self.states[key] = current
                continue
            edge = pressed != prior["pressed"]
            edge_name = "press" if pressed else "release"
            edge_field = "pressedAt" if pressed else "releasedAt"
            for field in ("pressedAt", "releasedAt"):
                old, new = prior[field], current[field]
                if old is not None and new is not None and new < old:
                    self.failures.append({"key": key, "type": "regressing", "field": field, "previous": old, "current": new, "receiveNs": receive_ns})
                expected_field = edge_field if edge else None
                if new != old and field != expected_field:
                    self.failures.append({"key": key, "type": "changedWithoutEdge", "field": field, "previous": old, "current": new, "receiveNs": receive_ns})
            if edge:
                timestamp = current[edge_field]
                if timestamp is None:
                    self.failures.append({"key": key, "type": "missing", "field": edge_field, "receiveNs": receive_ns})
                self.events.append({"key": key, "edge": edge_name, "apiGameplayMs": timestamp, "receiveNs": receive_ns, "count": count})
                if pressed:
                    prior["observedPresses"] += 1
                    prior["pendingPress"] = timestamp
                else:
                    prior["observedReleases"] += 1
                    if prior["pendingPress"] is not None and timestamp is not None:
                        prior["durations"].append(timestamp - prior["pendingPress"])
                    prior["pendingPress"] = None
            current.update(
                firstCount=prior["firstCount"], observedPresses=prior["observedPresses"],
                observedReleases=prior["observedReleases"], durations=prior["durations"],
                pendingPress=prior["pendingPress"],
                mode=prior["mode"], segment=prior["segment"],
            )
            self.states[key] = current

    def result(self) -> dict[str, Any]:
        summaries: dict[str, Any] = {}
        all_states = self.completed_states | self.states
        for key in sorted(all_states):
            state = all_states[key]
            delta = state["count"] - state["firstCount"]
            summaries[key] = {
                "mode": state["mode"], "segment": state["segment"],
                "firstCount": state["firstCount"], "lastCount": state["count"], "countDelta": delta,
                "observedPresses": state["observedPresses"], "observedReleases": state["observedReleases"],
                "estimatedMissedPresses": max(0, delta - state["observedPresses"]),
                "pressDurationsMs": state["durations"],
            }
        gaps = [later - earlier for earlier, later in zip(self.receive_times, self.receive_times[1:])]
        gap_stats = {
            "count": len(gaps), "min": min(gaps) if gaps else None, "max": max(gaps) if gaps else None,
            "mean": (sum(gaps) / len(gaps)) if gaps else None,
        }
        return {"keys": summaries, "receiveFrameGapsNs": gap_stats, "invariantFailures": self.failures, "frames": self.frames}


def capture(base_url: str, label: str, out: Path, timeout: float) -> dict[str, Any]:
    if not math.isfinite(timeout) or timeout <= 0:
        raise ValidationError("timeout must be a finite positive number")
    accounting = CaptureAccounting()
    started_wall = time.time_ns()
    ws = websocket_connect(base_url, timeout)
    deadline = time.monotonic() + timeout
    completed = False
    try:
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                completed = True
                break
            ws.sock.settimeout(remaining)
            try:
                message = ws.receive_message()
            except socket.timeout:
                completed = True
                break
            if message is None:
                completed = True
                break
            receive_ns = time.monotonic_ns()
            try:
                payload = json.loads(message)
            except (UnicodeDecodeError, json.JSONDecodeError) as exc:
                raise ValidationError(f"invalid precise JSON: {exc}") from exc
            accounting.process(payload, receive_ns)
    finally:
        ws.close()
    result = {
        "metadata": {"label": label, "baseUrl": base_url, "endpoint": "/websocket/v2/precise", "startedUnixNs": started_wall},
        "events": accounting.events,
        "summary": accounting.result(),
    }
    if completed:
        atomic_json_write(out, result)
    if not accounting.states and not accounting.completed_states:
        raise ValidationError("capture completed without keypress data")
    return result
