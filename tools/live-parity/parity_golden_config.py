"""The optional `golden` block of the live-parity run configuration (`golden-capture` only).

It names the owner authorization and the run-owned capture layout: a capture root
that contains every root the client declares, the route roots that the path-security
probes target, other declared roots (the lazer files slot), fixture values for the
route matrix and approved generated assets (GOLDEN_CONTRACT.md §2, §4, §6).
"""
from __future__ import annotations

import ntpath
import posixpath
import re
from typing import Any

AUTHORIZATION_KEYS = ("reference", "platform", "source_revision", "vosu_commit",
                      "client_version", "scenario_set")
ROUTE_PREFIXES = ("/Songs/", "/files/beatmap/", "/files/skin/")
FIXTURE_VALUES = ("songs_file", "songs_dir", "beatmap_relative", "skin_file", "calc_relative",
                  "calc_absolute_inside")
ASSET_KEYS = ("id", "relative", "media_type", "generator", "license")
REQUIRED = ("authorization", "capture_root", "capture_slot", "route_roots", "other_roots",
            "inside_files", "fixture_values", "assets")
OPTIONAL = ("sentinel_chart",)
_SLOT = r"^[A-Za-z0-9!_-][A-Za-z0-9!._-]{0,63}$"
_NAME = r"^[a-z0-9]+(-[a-z0-9]+)*$"


def golden_config_schema() -> dict[str, Any]:
    text = {"type": "string", "minLength": 1}
    strings = {"type": "object", "additionalProperties": text}
    return {
        "type": "object", "required": list(REQUIRED), "additionalProperties": False,
        "properties": {
            "authorization": {
                "type": "object", "required": list(AUTHORIZATION_KEYS),
                "properties": {key: text for key in AUTHORIZATION_KEYS},
                "additionalProperties": False,
            },
            "capture_root": text,
            "capture_slot": {"type": "string", "pattern": _SLOT},
            "route_roots": {**strings, "propertyNames": {"enum": list(ROUTE_PREFIXES)}},
            "other_roots": {**strings, "propertyNames": {"pattern": _NAME}},
            "inside_files": {**strings, "propertyNames": {"enum": list(ROUTE_PREFIXES)}},
            "fixture_values": {**strings, "propertyNames": {"enum": list(FIXTURE_VALUES)}},
            "sentinel_chart": text,
            "assets": {"type": "array", "items": {
                "type": "object", "required": list(ASSET_KEYS),
                "properties": {key: text for key in ASSET_KEYS},
                "additionalProperties": False,
            }},
        },
    }


def _strings(value: Any, where: str, keys: tuple[str, ...] | None, errors: list[str]) -> None:
    if not isinstance(value, dict):
        errors.append(f"{where} must be an object")
        return
    for key, item in value.items():
        if keys is not None and key not in keys:
            errors.append(f"{where} has unknown key {key!r}")
        if not isinstance(item, str) or not item:
            errors.append(f"{where}.{key} must be a non-empty string")


def validate_golden_config(value: Any, os_name: str) -> list[str]:
    """Shape errors of a `golden` block; empty when valid. Paths are checked again,
    against the filesystem, when golden-capture prepares the layout."""
    if not isinstance(value, dict):
        return ["golden must be an object"]
    errors = [f"golden.{key} is required" for key in REQUIRED if key not in value]
    errors += [f"golden has unknown field {key!r}" for key in value
               if key not in REQUIRED + OPTIONAL]
    pm = ntpath if os_name == "windows" else posixpath
    authorization = value.get("authorization")
    _strings(authorization, "golden.authorization", AUTHORIZATION_KEYS, errors)
    if isinstance(authorization, dict):
        errors += [f"golden.authorization.{key} is required"
                   for key in AUTHORIZATION_KEYS if key not in authorization]
    root = value.get("capture_root")
    if not isinstance(root, str) or not pm.isabs(root) or pm.normpath(root) != root:
        errors.append("golden.capture_root must be an absolute normalized path")
    if not isinstance(value.get("capture_slot"), str) \
            or re.match(_SLOT, value["capture_slot"]) is None:
        errors.append("golden.capture_slot must be a synthetic root slot name")
    for key, names in (("route_roots", ROUTE_PREFIXES), ("other_roots", None)):
        _strings(value.get(key), f"golden.{key}", names, errors)
        for name, path in (value.get(key) or {}).items() if isinstance(value.get(key), dict) else ():
            if names is None and re.match(_NAME, name) is None:
                errors.append(f"golden.{key} slot {name!r} is not a lowercase name")
            if isinstance(path, str) and (not pm.isabs(path) or pm.normpath(path) != path):
                errors.append(f"golden.{key}.{name} must be an absolute normalized path")
    _strings(value.get("inside_files"), "golden.inside_files", ROUTE_PREFIXES, errors)
    _strings(value.get("fixture_values"), "golden.fixture_values", FIXTURE_VALUES, errors)
    chart = value.get("sentinel_chart")
    if chart is not None and (not isinstance(chart, str) or not pm.isabs(chart)):
        errors.append("golden.sentinel_chart must be an absolute path")
    assets = value.get("assets")
    if not isinstance(assets, list):
        errors.append("golden.assets must be an array")
    else:
        for index, asset in enumerate(assets):
            _strings(asset, f"golden.assets[{index}]", ASSET_KEYS, errors)
            if isinstance(asset, dict):
                errors += [f"golden.assets[{index}].{key} is required"
                           for key in ASSET_KEYS if key not in asset]
    return errors
