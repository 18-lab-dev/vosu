"""Reviewed replay asset declaration corrections, never captured-data edits."""
from __future__ import annotations

import base64
import re
from collections.abc import Iterator
from typing import Any
from urllib.parse import unquote

from parity_golden_rules import GoldenError
from parity_volatile import VolatileError, cbor_text_fields

_ASSET_ID = re.compile(r"[a-z0-9]+(?:-[a-z0-9]+)*(?:\.[a-z0-9]+(?:-[a-z0-9]+)*)*")
_MIME_NAME = r"[a-z0-9][a-z0-9!#$&^_.+-]{0,126}"
_MEDIA_TYPE = re.compile(rf"{_MIME_NAME}/{_MIME_NAME}")
_RESOURCE_FILES = {"file": "beatmapFile", "audio": "audio", "background": "background"}


class Corrections:
    def __init__(self, corrections: list[tuple[str, str]], review_receipt: str | None):
        self.review_receipt = review_receipt
        self.requested: dict[str, tuple[str, str]] = {}
        self.applied: set[str] = set()
        if corrections and review_receipt is None:
            raise GoldenError("asset media type correction requires --review-receipt")
        for declaration, reason in corrections:
            asset_id, _, media_type = declaration.partition("=")
            if len(asset_id) > 120 or not _ASSET_ID.fullmatch(asset_id):
                raise GoldenError("asset media type correction requires an asset ID, not a field")
            if not _MEDIA_TYPE.fullmatch(media_type):
                raise GoldenError("asset media type correction requires a bare lowercase media type")
            if not reason.strip() or not reason.isprintable() or len(reason) > 512:
                raise GoldenError("asset media type correction requires a short printable reason")
            if asset_id in self.requested:
                raise GoldenError("duplicate asset media type correction")
            self.requested[asset_id] = media_type, reason

    def apply(self, assets: dict[str, dict[str, Any]], cases: list[dict[str, Any]],
              *, lazer: bool) -> list[dict[str, Any]]:
        result = []
        for asset_id in sorted(self.requested.keys() & assets.keys()):
            if asset_id in self.applied:
                raise GoldenError("asset media type correction is ambiguous across platforms")
            corrected, reason = self.requested[asset_id]
            asset = assets[asset_id]
            original = asset["media_type"]
            if original == corrected:
                raise GoldenError(f"asset {asset_id}: media type correction is a no-op")
            if any(kind != corrected for kind in _recorded_types(asset, cases, lazer)):
                raise GoldenError(f"asset {asset_id}: correction disagrees with recorded vosu media type")
            asset["media_type"] = corrected
            self.applied.add(asset_id)
            result.append({"asset_id": asset_id, "original": original, "corrected": corrected,
                           "reason": reason, "review_receipt": self.review_receipt})
        return result

    def finish(self) -> None:
        if self.requested.keys() - self.applied:
            raise GoldenError("asset media type correction names an unknown asset ID")


def _cbor(entry: dict[str, Any], *path: str) -> tuple[str | None, ...]:
    try:
        return cbor_text_fields(base64.b64decode(entry["value_cbor_b64"]), *path)
    except VolatileError as exc:
        raise GoldenError("invalid dump CBOR while checking asset media type correction") from exc


def _recorded_types(asset: dict[str, Any], cases: list[dict[str, Any]],
                    lazer: bool) -> Iterator[str]:
    game_slot = "files" if lazer else "songs"
    for case in cases:
        # Only vouches for the file at the same retained slot and relative path;
        # identical asset bytes (e.g. the calculator copy) do not identify it.
        if asset["slot"] == game_slot:
            for dump in case["dumps"]:
                entries = {(e["ns"], e["pointer"]): e for e in dump["entries"]
                           if e["value_cbor_b64"] is not None}
                paths = entries.get(("vosu.osu", "/internal/paths"))
                for kind, leaf in _RESOURCE_FILES.items():
                    ref = entries.get(("vosu.chart", f"/resources/{kind}"))
                    if ref is None:
                        continue
                    if paths is None:
                        raise GoldenError("recorded chart resource has no paths to identify its asset")
                    folder, filename = _cbor(paths, "/current/beatmapFolder", f"/current/{leaf}")
                    if not filename:
                        raise GoldenError("recorded chart resource has no filename to identify its asset")
                    relative = filename if lazer or not folder else f"{folder}/{filename}"
                    if relative.replace("\\", "/") == asset["relative"]:
                        media_type, = _cbor(ref, "/type")
                        if not media_type:
                            raise GoldenError("recorded chart resource has no media type")
                        yield media_type
        route = case["route"]
        method, target, _ = route["request"].split("\r\n", 1)[0].split(" ")
        if method not in ("GET", "HEAD") or route["status"] not in (200, 206):
            continue
        path = target.partition("?")[0]
        for prefix, slot in (("/Songs/", game_slot), ("/files/beatmap/", game_slot),
                             ("/files/skin/", "skin")):
            if slot != asset["slot"] or not path.startswith(prefix):
                continue
            child = unquote(path[len(prefix):])
            # Shortcut response MIME is a route-specific wire field, not a
            # retained asset declaration. Its resource reference above is proof.
            if prefix == "/files/beatmap/" and child in _RESOURCE_FILES:
                continue
            if child == asset["relative"]:
                for name, value in route["headers"]:
                    if name.lower() == "content-type" and value.strip():
                        yield value.partition(";")[0].strip().lower()
