"""`golden-capture` barrier wait/observe logic: traversing menu -> play -> paused ->
result -> no-client, and comparing both daemons' reported client state at each one.

Split out of `parity_golden_capture.py` (docs/architecture/source-map.md's 1,000-line
maintained-file limit). `Session` mixes in `BarrierMixin` for
`_observe`/`_holds`/`_wait_barrier`. GOLDEN_CONTRACT.md §6 is normative for the
barrier order and the play barrier's pre-gameplay pause tolerance.
"""
from __future__ import annotations

from dataclasses import dataclass
import json
import math
import time
from typing import TYPE_CHECKING, Any

from live_parity import Sample, barrier_of, beatmap_time
from parity_probes import HttpProbe
import parity_volatile as volatile

if TYPE_CHECKING:
    from parity_golden_capture import Daemon, HarnessDump

TRAVERSAL = ("menu", "play", "paused", "result", "no-client")
# A barrier holds once both daemons report it across this many consecutive observations.
# A barrier with cases also needs the non-volatile views of the upstream v2 payload and
# the vosu v2 dump to stay identical across them; a case-less one (play: its clock and
# the provider generation advance on every read) needs only the reported state.
STABLE_OBSERVATIONS = 3
POLL_S = 0.1
# The pinned upstream waits up to 15 s for lazer startup after discovering it.
UPSTREAM_READY_TIMEOUT_S = 30.0
MAIN_MENU_FFT_TIMEOUT_S = 30.0
OPERATOR_STEPS = {
    "menu": "open song select on the approved map (menu)",
    "play": "resume the song-select track, then start the approved map (active play)",
    "paused": "pause gameplay and leave it paused",
    "result": "resume, finish the map and stay on the result screen",
    "no-client": "close the client",
}


class CaptureError(ValueError):
    """golden-capture cannot produce a complete attested capture."""


def _text(payload: Any, section: str, key: str) -> str:
    block = payload.get(section) if isinstance(payload, dict) else None
    value = block.get(key) if isinstance(block, dict) else None
    return value if isinstance(value, str) and value.strip() else ""


@dataclass(frozen=True)
class ClientFields:
    checksum: str
    path: str
    game_folder: str

    @classmethod
    def read(cls, payload: Any) -> ClientFields:
        path = _text(payload, "directPath", "beatmapFile")
        return cls(_text(payload, "beatmap", "checksum").strip().lower(), "" if path == "." else path,
                   _text(payload, "folders", "game"))

    def missing(self) -> list[str]:
        return (["beatmap.checksum or directPath.beatmapFile"] if not (self.checksum or self.path)
                else []) + (["folders.game"] if not self.game_folder else [])

    def same_beatmap(self, other: ClientFields) -> bool:
        if self.checksum and other.checksum:
            return self.checksum == other.checksum
        return bool(self.path and self.path == other.path)


@dataclass(frozen=True)
class BarrierObservation:
    seen: dict[str, str | None]
    view: Any
    clock: float | None
    source: ClientFields
    vosu: ClientFields
    main_menu_fft: tuple[bool, bool] = (False, False)


def main_menu_fft(payload: Any) -> bool:
    if not isinstance(payload, dict):
        return False
    state, settings = payload.get("state"), payload.get("settings")
    if not isinstance(state, dict) or type(state.get("number")) is not int or state["number"] != 0:
        return False
    density = settings.get("bassDensity") if isinstance(settings, dict) else None
    if type(density) is int:
        return density > 0
    return type(density) is float and 0 < density and math.isfinite(density)


class BarrierMixin:
    """`Session`'s barrier observe/wait methods (parity_golden_capture.py); expects the
    instance attributes `transcript`, `client`, `deadline` and `operator` it defines."""

    def _observe(self, source: Daemon, dump: HarnessDump) -> BarrierObservation:
        """Barrier, non-volatile v2 view, upstream clock and attachment fields."""
        with HttpProbe(f"http://127.0.0.1:{source.port}", "source", "v2", source.evidence) as probe:
            sample = probe.fetch()
        upstream = barrier_of(sample)
        clock = beatmap_time(sample.payload)
        envelope = dump.fetch("v2")
        payload = None
        if envelope["http"]["kind"] == "not_ready":
            vosu = "process-exit"
        else:
            try:
                payload = json.loads(envelope["http"]["body_utf8"])
            except ValueError:
                payload = None
            vosu = barrier_of(Sample("vosu", "v2", 0, payload))
        rename = {"process-exit": "no-client"}
        try:
            view = (volatile.payload_view("/json/v2", json.dumps(sample.payload).encode("utf-8"))[0],
                    volatile.dump_view(envelope)[0])
        except volatile.VolatileError:
            view = None  # never stable: a volatile field broke its rule
        return BarrierObservation({"source": rename.get(upstream, upstream), "vosu": rename.get(vosu, vosu)},
                                  view, clock, ClientFields.read(sample.payload), ClientFields.read(payload),
                                  (main_menu_fft(sample.payload), main_menu_fft(payload)))

    def _holds(self, barrier: str, observation: BarrierObservation) -> bool:
        """An attested live client with two matching reads, or an observed client exit."""
        no_client = barrier == "no-client"
        return (self.client.exited() == no_client
                and all(value == barrier for value in observation.seen.values())
                and (no_client or (not observation.source.missing() and not observation.vosu.missing()
                                   and observation.source.same_beatmap(observation.vosu))))

    def _wait_main_menu_fft(self, source: Daemon, dump: HarnessDump) -> None:
        self.operator("main-menu", "waiting for main-menu FFT: stay on the main menu with music playing "
                                   "until both daemons have sampled it")
        deadline = min(self.deadline, time.monotonic() + MAIN_MENU_FFT_TIMEOUT_S)
        ready = (False, False)
        while time.monotonic() < deadline and not self.client.exited():
            ready = self._observe(source, dump).main_menu_fft
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            if all(ready):
                self.transcript["main_menu_fft"] = {"state": 0, "source": True, "vosu": True,
                                                    "monotonic_ns": time.monotonic_ns()}
                return
            time.sleep(min(POLL_S, remaining))
        raise CaptureError("main_menu_fft_not_observed: both daemons must report positive "
                           f"bassDensity at state 0 within {MAIN_MENU_FFT_TIMEOUT_S:g} s "
                           f"(source={ready[0]}, vosu={ready[1]})")

    def _wait_barrier(self, barrier: str, source: Daemon, dump: HarnessDump,
                      reached: list[str], *, identical_views: bool = True) -> None:
        index = TRAVERSAL.index(barrier)
        self.operator(barrier, f"waiting for barrier {barrier}: {OPERATOR_STEPS[barrier]}")
        stable, previous = 0, None
        # The player loader reports `play` with a clock that has not advanced yet, and
        # upstream's own rule (`game.paused = playTime == previousPlayTime`) reports that
        # as `paused` before gameplay starts (GOLDEN_CONTRACT.md "Volatile ledger and
        # stable barriers"). Tolerate only that one case: an upstream `paused` observed
        # while waiting for `play`, before its clock has ever advanced since this wait
        # began. "Gameplay started" is defined purely from that observed clock, so once
        # it has advanced even once, a later `paused` is a real out-of-order observation
        # again, like a vosu-side `paused` at any time: both still abort.
        last_clock: float | None = None
        gameplay_started = False
        pregame_pauses = 0
        unready_since: float | None = None
        while stable < STABLE_OBSERVATIONS:
            if time.monotonic() > self.deadline:
                raise CaptureError(f"barrier {barrier} was not reached before the timeout")
            observation = self._observe(source, dump)
            seen, view, clock = observation.seen, observation.view, observation.clock
            if barrier == "play" and clock is not None:
                if last_clock is not None and clock != last_clock:
                    gameplay_started = True
                last_clock = clock
            for daemon, value in seen.items():
                if value not in TRAVERSAL or TRAVERSAL.index(value) <= index:
                    continue
                if value == "no-client" and not reached:
                    continue  # The client is still starting.
                if (barrier == "play" and value == "paused" and daemon == "source"
                        and not gameplay_started):
                    pregame_pauses += 1
                    continue  # The player loader's frozen clock, not a real pause.
                raise CaptureError(f"barrier out of order: {daemon} reached {value} while "
                                   f"waiting for {barrier} after {reached or ['launch']}")
            readiness_error = None
            if barrier != "no-client" and not observation.vosu.missing():
                source_missing = observation.source.missing()
                if source_missing:
                    readiness_error = f"missing {', '.join(source_missing)}"
                elif not observation.source.same_beatmap(observation.vosu):
                    readiness_error = "beatmap.checksum or directPath.beatmapFile does not match vosu"
            if readiness_error is not None:
                now = time.monotonic()
                if unready_since is None:
                    unready_since = now
                elif now - unready_since >= UPSTREAM_READY_TIMEOUT_S:
                    raise CaptureError(f"upstream_not_reading_client: waiting for {barrier}; "
                                       f"{readiness_error} after "
                                       f"{UPSTREAM_READY_TIMEOUT_S:g} s with vosu ready")
            else:
                unready_since = None
            if not self._holds(barrier, observation) or (identical_views and view is None):
                stable, previous = 0, None
            elif not identical_views:
                stable += 1
            else:
                stable = stable + 1 if stable and view == previous else 1
                previous = view
            if stable < STABLE_OBSERVATIONS:
                time.sleep(POLL_S)
        reached.append(barrier)
        self.transcript["barriers"].append({"barrier": barrier, "monotonic_ns": time.monotonic_ns(),
                                            "stable_observations": STABLE_OBSERVATIONS,
                                            "identical_views": identical_views,
                                            "pregame_pauses_tolerated": pregame_pauses})
