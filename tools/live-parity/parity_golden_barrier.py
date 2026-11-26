"""`golden-capture` barrier wait/observe logic: traversing menu -> play -> paused ->
result -> no-client, and comparing both daemons' reported client state at each one.

Split out of `parity_golden_capture.py` (docs/architecture/source-map.md's 1,000-line
maintained-file limit) because that module was already at the limit; `Session` mixes
in `BarrierMixin` for `_observe`/`_holds`/`_wait_barrier`, so this is purely a module
boundary, not a behavior change. GOLDEN_CONTRACT.md §6 is normative for the barrier
order and the play barrier's pre-gameplay pause tolerance documented there.
"""
from __future__ import annotations

import json
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
OPERATOR_STEPS = {
    "menu": "open song select on the approved map (menu)",
    "play": "resume the song-select track, then start the approved map (active play)",
    "paused": "pause gameplay and leave it paused",
    "result": "resume, finish the map and stay on the result screen",
    "no-client": "close the client",
}


class CaptureError(ValueError):
    """golden-capture cannot produce a complete attested capture."""


class BarrierMixin:
    """`Session`'s barrier observe/wait methods (parity_golden_capture.py); expects the
    instance attributes `transcript`, `client`, `deadline` and `operator` it defines."""

    def _observe(self, source: Daemon, dump: HarnessDump) -> tuple[dict[str, str | None], Any, float | None]:
        """Both daemons' barriers, the non-volatile view of their v2 state, and the
        source's live beatmap clock (None when the payload carries none)."""
        with HttpProbe(f"http://127.0.0.1:{source.port}", "source", "v2", source.evidence) as probe:
            sample = probe.fetch()
        upstream = barrier_of(sample)
        clock = beatmap_time(sample.payload)
        envelope = dump.fetch("v2")
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
        return {"source": rename.get(upstream, upstream), "vosu": rename.get(vosu, vosu)}, view, clock

    def _holds(self, barrier: str, seen: dict[str, str | None]) -> bool:
        """Both daemons report the barrier, and the attested client is alive in a client
        barrier and its exit was observed for no-client (unapproved clients raise)."""
        exited = self.client.exited()
        return exited == (barrier == "no-client") and all(value == barrier for value in seen.values())

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
        while stable < STABLE_OBSERVATIONS:
            if time.monotonic() > self.deadline:
                raise CaptureError(f"barrier {barrier} was not reached before the timeout")
            seen, view, clock = self._observe(source, dump)
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
            if not self._holds(barrier, seen) or (identical_views and view is None):
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
