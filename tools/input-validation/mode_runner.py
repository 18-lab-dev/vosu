from __future__ import annotations

import math
import random
import sys
import time
from collections.abc import Callable
from dataclasses import dataclass

from input_validation import (
    DryRunBackend,
    KeyboardBackend,
    ValidationError,
    create_keyboard_backend,
    normalize_key_name,
    parse_key_mapping,
    run_live_timeline,
)
from mode_plans import Action, KeyAction, PlayPlan, PointerMove
from pointer_backends import (
    DryRunPointerBackend,
    PlayfieldRect,
    PointerBackend,
    PointerBackendError,
    create_pointer_backend,
)


_DEFAULT_CONTROLS = {
    0: ("Z",),
    # PlayPlan order: DON_LEFT, DON_RIGHT, KAT_LEFT, KAT_RIGHT.
    1: ("F", "J", "D", "K"),
}


@dataclass(slots=True)
class PlanBackends:
    keyboard: KeyboardBackend | None
    pointer: PointerBackend | None


def parse_rectangle(value: str | None, option: str) -> PlayfieldRect | None:
    if value is None:
        return None
    parts = value.split(",")
    if len(parts) != 4:
        raise ValidationError(f"{option} must contain x,y,width,height")
    try:
        return PlayfieldRect(*(int(part.strip()) for part in parts))
    except (TypeError, ValueError) as exc:
        raise ValidationError(f"invalid {option}: {exc}") from exc


def key_mapping_for_plan(plan: PlayPlan, value: str | None) -> tuple[str, ...]:
    if not plan.controls:
        if value is not None:
            raise ValidationError("--keys is not valid for a pointer-only catch plan")
        return ()
    if value is None and plan.mode in _DEFAULT_CONTROLS:
        return _DEFAULT_CONTROLS[plan.mode]
    return parse_key_mapping(value, len(plan.controls))


def create_plan_backends(
    plan: PlayPlan,
    backend_name: str,
    keys: tuple[str, ...],
    start_key: str | None,
    playfield: PlayfieldRect | None,
    desktop: PlayfieldRect | None,
) -> PlanBackends:
    needs_pointer = any(isinstance(action, PointerMove) for action in plan.actions)
    if needs_pointer and playfield is None:
        if backend_name == "dry-run":
            playfield = PlayfieldRect(0, 0, 513, 385)
        else:
            raise ValidationError("--playfield is required for osu! and catch plans")

    keyboard: KeyboardBackend | None = None
    pointer: PointerBackend | None = None
    backend_keys = tuple(dict.fromkeys(keys + (() if start_key is None else (start_key,))))
    try:
        if backend_keys:
            keyboard = create_keyboard_backend(backend_name, backend_keys)
        if needs_pointer:
            assert playfield is not None
            pointer = create_pointer_backend(backend_name, playfield, desktop)
        return PlanBackends(keyboard, pointer)
    except (PointerBackendError, TypeError, ValueError) as exc:
        if keyboard is not None:
            keyboard.close()
        raise ValidationError(str(exc)) from exc
    except BaseException:
        if keyboard is not None:
            keyboard.close()
        raise


def _close_backends(backends: PlanBackends, pressed: set[str]) -> None:
    active_error = sys.exc_info()[1]
    cleanup_error: BaseException | None = None
    if backends.keyboard is not None:
        for key in sorted(pressed):
            try:
                backends.keyboard.emit(key, False)
            except BaseException as exc:
                if cleanup_error is None:
                    cleanup_error = exc
        try:
            backends.keyboard.close()
        except BaseException as exc:
            if cleanup_error is None:
                cleanup_error = exc
    if backends.pointer is not None:
        try:
            backends.pointer.close()
        except BaseException as exc:
            if cleanup_error is None:
                cleanup_error = exc
    if cleanup_error is not None and active_error is None:
        raise cleanup_error


def _action_emitter(
    plan: PlayPlan,
    keys: tuple[str, ...],
    backends: PlanBackends,
    pressed: set[str],
) -> Callable[[Action], None]:
    mapping = dict(zip(plan.controls, keys, strict=True))

    def emit(action: Action) -> None:
        if isinstance(action, PointerMove):
            if backends.pointer is None:
                raise ValidationError("plan requires a pointer backend")
            backends.pointer.move(action.x, action.y)
            return
        if backends.keyboard is None:
            raise ValidationError("plan requires a keyboard backend")
        key = mapping[action.control]
        backends.keyboard.emit(key, action.pressed)
        if action.pressed:
            pressed.add(key)
        else:
            pressed.discard(key)

    return emit


def _dry_run(plan: PlayPlan, keys: tuple[str, ...], output: Callable[[str], None]) -> None:
    mapping = dict(zip(plan.controls, keys, strict=True))
    for action in plan.actions:
        if isinstance(action, PointerMove):
            output(
                f"{action.timestamp_ms:>7} ms  POINTER "
                f"{action.x:.3f},{action.y:.3f}"
            )
        else:
            state = "down" if action.pressed else "up"
            output(
                f"{action.timestamp_ms:>7} ms  {action.control} "
                f"{mapping[action.control]} {state}"
            )


def play_plan(
    plan: PlayPlan,
    keys: tuple[str, ...],
    backends: PlanBackends,
    start_delay: float,
    *,
    output: Callable[[str], None] = print,
    perf_counter_ns: Callable[[], int] = time.perf_counter_ns,
    sleep: Callable[[float], None] = time.sleep,
) -> None:
    pressed: set[str] = set()
    try:
        if not math.isfinite(start_delay) or start_delay < 0:
            raise ValidationError("start delay must be a finite non-negative number")
        if len(keys) != len(plan.controls):
            raise ValidationError("key mapping does not match plan controls")
        if isinstance(backends.keyboard, DryRunBackend) or isinstance(
            backends.pointer, DryRunPointerBackend
        ):
            _dry_run(plan, keys, output)
            return

        emit = _action_emitter(plan, keys, backends, pressed)
        origin_ns = perf_counter_ns() + round(start_delay * 1_000_000_000)
        for action in plan.actions:
            target_ns = origin_ns + action.timestamp_ms * 1_000_000
            while True:
                remaining_ns = target_ns - perf_counter_ns()
                if remaining_ns <= 0:
                    break
                if remaining_ns > 2_000_000:
                    sleep((remaining_ns - 1_000_000) / 1_000_000_000)
            emit(action)
    finally:
        _close_backends(backends, pressed)


def jittered_plan_actions(
    plan: PlayPlan,
    jitter_ms: float,
    seed: int,
) -> tuple[tuple[float, Action], ...]:
    if not math.isfinite(jitter_ms) or jitter_ms < 0:
        raise ValidationError("jitter must be a finite non-negative number")
    if jitter_ms and plan.mode != 3:
        raise ValidationError("timing jitter is supported only for osu!mania plans")
    generator = random.Random(seed)
    active_offsets: dict[str, float] = {}
    scheduled: list[tuple[float, Action]] = []
    previous = -math.inf
    for action in plan.actions:
        offset = 0.0
        if isinstance(action, KeyAction):
            if action.pressed:
                offset = generator.uniform(-jitter_ms, jitter_ms) if jitter_ms else 0.0
                active_offsets[action.control] = offset
            else:
                offset = active_offsets.pop(action.control, 0.0)
        timestamp = max(previous, action.timestamp_ms + offset)
        scheduled.append((timestamp, action))
        previous = timestamp
    return tuple(scheduled)


def live_play_plan(
    plan: PlayPlan,
    keys: tuple[str, ...],
    backends: PlanBackends,
    base_url: str,
    start_key: str | None,
    start_timeout: float,
    jitter_ms: float = 0.0,
    seed: int = 0,
    *,
    sleep: Callable[[float], None] = time.sleep,
) -> None:
    pressed: set[str] = set()
    try:
        if len(keys) != len(plan.controls):
            raise ValidationError("key mapping does not match plan controls")
        if start_key is not None:
            start_key = normalize_key_name(start_key, "start key")
            if backends.keyboard is None:
                raise ValidationError("start key requires a keyboard backend")

        emit = _action_emitter(plan, keys, backends, pressed)

        def start() -> None:
            assert start_key is not None and backends.keyboard is not None
            pressed.add(start_key)
            backends.keyboard.emit(start_key, True)
            sleep(0.04)
            backends.keyboard.emit(start_key, False)
            pressed.discard(start_key)

        scheduled = jittered_plan_actions(plan, jitter_ms, seed)
        run_live_timeline(
            scheduled,
            emit,
            base_url,
            start if start_key is not None else None,
            start_timeout,
        )
    finally:
        _close_backends(backends, pressed)
