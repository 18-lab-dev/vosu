"""Run-owned sentinel layout and abort-before-send checks for path-security probes.

The pinned upstream daemon has broad file access, so a traversal probe may only
target a sentinel this run created inside a temporary capture root. Every
spelling is checked lexically under the platform's path rules and, when the
probe runs on that platform, against the real filesystem before any byte is
sent (GOLDEN_CONTRACT.md §6, plan §3.7).
"""
from __future__ import annotations

import dataclasses
from dataclasses import dataclass, field
import hashlib
import ntpath
import os
from pathlib import Path
import posixpath
import re
import secrets
import shutil
import sys
from typing import Mapping
from urllib.parse import quote, unquote

MARKER_NAME = ".vosu-route-probe-root"
SENTINEL_DIR = "vosu-route-probe"
SENTINEL_FILE = "sentinel.txt"
SENTINEL_CHART = "sentinel.osu"
LINK_NAME = "vosu-route-probe-link"
ROUTE_PREFIXES = ("/Songs/", "/files/beatmap/", "/files/skin/")
PATH_SAFE = "!$&'()*+,;=:@"

OUTSIDE, INSIDE, LINK = "outside", "inside", "link"


@dataclass(frozen=True)
class Spelling:
    name: str
    kind: str
    oses: frozenset[str]


SPELLINGS = (
    Spelling("encoded-parent", OUTSIDE, frozenset({"windows", "linux"})),
    Spelling("dot-segment", OUTSIDE, frozenset({"windows", "linux"})),
    Spelling("double-encoded-parent", OUTSIDE, frozenset({"windows", "linux"})),
    Spelling("backslash-encoded", OUTSIDE, frozenset({"windows", "linux"})),
    Spelling("backslash-raw", OUTSIDE, frozenset({"windows", "linux"})),
    Spelling("absolute-encoded", OUTSIDE, frozenset({"linux"})),
    Spelling("absolute-double-slash", OUTSIDE, frozenset({"linux"})),
    Spelling("absolute-drive", OUTSIDE, frozenset({"windows"})),
    Spelling("absolute-drive-slash", OUTSIDE, frozenset({"windows"})),
    Spelling("unc-extended", OUTSIDE, frozenset({"windows"})),
    Spelling("unc-device", OUTSIDE, frozenset({"windows"})),
    Spelling("ads", INSIDE, frozenset({"windows"})),
    Spelling("trailing-dot", INSIDE, frozenset({"windows"})),
    Spelling("trailing-space", INSIDE, frozenset({"windows"})),
    Spelling("symlink", LINK, frozenset({"windows", "linux"})),
)
SPELLING_BY_NAME = {spelling.name: spelling for spelling in SPELLINGS}


class SecurityAbort(ValueError):
    """A probe target is not a run-owned sentinel inside the capture root; nothing was sent."""


@dataclass(frozen=True)
class SecurityLayout:
    """Run-owned roots and sentinels. `os_name` is `windows` or `linux`."""

    os_name: str
    capture_root: str
    token: str
    route_roots: Mapping[str, str]
    inside_files: Mapping[str, str]
    sentinel: str
    sentinel_chart: str | None = None
    sentinel_chart_sha256: str | None = None
    links: Mapping[str, str] = field(default_factory=dict)
    link_errors: Mapping[str, str] = field(default_factory=dict)
    # Declared roots without a legacy file route (e.g. the lazer files slot used by
    # calculation), keyed by slot name.
    other_roots: Mapping[str, str] = field(default_factory=dict)

    def declared_roots(self) -> list[tuple[str, str]]:
        return [*self.route_roots.items(), *self.other_roots.items()]


def host_os() -> str:
    return "windows" if sys.platform == "win32" else "linux"


def _pm(os_name: str):
    return ntpath if os_name == "windows" else posixpath


def _norm(pm, path: str) -> str:
    value = pm.normcase(pm.normpath(path))
    if pm is posixpath and value.startswith("//"):
        value = "/" + value.lstrip("/")
    return value


def _within(pm, child: str, parent: str) -> bool:
    child, parent = _norm(pm, child), _norm(pm, parent)
    return child == parent or child.startswith(parent.rstrip(pm.sep) + pm.sep)


def _absolute(pm, path: str) -> bool:
    if pm is ntpath:
        drive, rest = ntpath.splitdrive(path)
        return bool(drive) and rest.startswith(("\\", "/"))
    return path.startswith("/")


_WINDOWS_HOME = re.compile(r"^[a-z]:\\users(\\[^\\]+)?$")
_POSIX_HOME = re.compile(r"^/(home(/[^/]+)?|root|users(/[^/]+)?|tmp|var|etc|usr|opt)$")


def _check_capture_root(layout: SecurityLayout) -> None:
    pm = _pm(layout.os_name)
    root = layout.capture_root
    if not _absolute(pm, root):
        raise SecurityAbort("capture root is not absolute")
    normalized = _norm(pm, root)
    anchor = _norm(pm, pm.splitdrive(root)[0] + pm.sep)
    pattern = _WINDOWS_HOME if pm is ntpath else _POSIX_HOME
    if normalized == anchor or pattern.match(normalized):
        raise SecurityAbort("capture root is a filesystem, home or system root")
    if layout.os_name == host_os():
        if _within(pm, os.path.realpath(Path.home()), os.path.realpath(root)):
            raise SecurityAbort("capture root contains the home directory")
    for name, declared in layout.declared_roots():
        if name.startswith("/") and name not in ROUTE_PREFIXES:
            raise SecurityAbort(f"unknown route prefix {name!r}")
        if not _absolute(pm, declared) or not _within(pm, declared, root) \
                or _norm(pm, declared) == normalized:
            raise SecurityAbort(f"declared root {name} is not strictly inside the capture root")
    for path in (layout.sentinel, layout.sentinel_chart):
        if path is None:
            continue
        if not _within(pm, path, root):
            raise SecurityAbort("sentinel is outside the capture root")
        if any(_within(pm, path, declared) for _, declared in layout.declared_roots()):
            raise SecurityAbort("sentinel is inside a declared root")


def _relative_path(value: str) -> list[str]:
    parts = value.split("/")
    if not value or value.startswith("/") or "\\" in value or any(
            part in ("", ".", "..") or any(ord(ch) < 32 or ord(ch) == 127 for ch in part)
            for part in parts):
        raise SecurityAbort(f"fixture path {value!r} is not a plain relative path")
    return parts


def encode_relative(value: str) -> str:
    """Percent-encode a plain '/'-separated relative fixture path, segment by segment."""
    return "/".join(quote(part, safe=PATH_SAFE) for part in _relative_path(value))


def _spell(spelling: str, layout: SecurityLayout, prefix: str) -> tuple[str, str]:
    """Return (raw request path suffix, intended absolute filesystem target)."""
    pm = _pm(layout.os_name)
    root = layout.route_roots[prefix]
    kind = SPELLING_BY_NAME[spelling].kind
    if kind == INSIDE:
        relative = layout.inside_files.get(prefix)
        if relative is None:
            raise SecurityAbort(f"no fixture file inside {prefix} for {spelling}")
        encoded = encode_relative(relative)
        target = pm.join(root, *_relative_path(relative))
        suffix = {"ads": "::$DATA", "trailing-dot": ".", "trailing-space": "%20"}[spelling]
        return encoded + suffix, target
    if kind == LINK:
        link = layout.links.get(prefix)
        if link is None:
            raise SecurityAbort(f"no symlink fixture under {prefix}: "
                                f"{layout.link_errors.get(prefix, 'not created')}")
        return encode_relative(link), pm.join(root, *_relative_path(link))
    sentinel = layout.sentinel
    parts = pm.relpath(sentinel, root).replace("\\", "/").split("/")
    if parts[0] != "..":
        raise SecurityAbort("sentinel is not outside its declared root")
    segments = [quote(part, safe=PATH_SAFE) for part in parts]
    joined = {
        "encoded-parent": "%2F", "dot-segment": "/", "double-encoded-parent": "%252F",
        "backslash-encoded": "%5C", "backslash-raw": "\\",
    }.get(spelling)
    if joined is not None:
        return joined.join(segments), sentinel
    absolute = {
        "absolute-encoded": quote(sentinel, safe=""),
        "absolute-double-slash": quote(sentinel, safe="/"),
        "absolute-drive": quote(sentinel, safe=""),
        "absolute-drive-slash": quote(sentinel.replace("\\", "/"), safe="/:"),
        "unc-extended": quote("\\\\?\\" + sentinel, safe=""),
        "unc-device": quote("\\\\.\\" + sentinel, safe=""),
    }[spelling]
    return absolute, sentinel


def _interpretations(pm, spelling: str, root: str) -> list[str]:
    """Every naive server interpretation: up to two percent-decodes, either separator."""
    decoded, current = [], spelling
    for _ in range(3):
        decoded.append(current)
        current = unquote(current)
    paths = []
    for value in dict.fromkeys(decoded):
        for variant in dict.fromkeys((value, value.replace("\\", "/"))):
            stripped = variant
            for device in ("//?/", "//./", "\\\\?\\", "\\\\.\\"):
                if stripped.startswith(device):
                    stripped = stripped[len(device):]
            if pm is ntpath and re.match(r"^[A-Za-z]:(?![\\/])", stripped):
                raise SecurityAbort("drive-relative spelling cannot be bounded")
            if pm is ntpath and stripped.startswith(("//", "\\\\")):
                raise SecurityAbort("UNC network spelling is never probed")
            paths.append(stripped if _absolute(pm, stripped) else pm.join(root, stripped))
    return paths


def _host_checks(layout: SecurityLayout, kind: str, target: str, interpretations: list[str],
                 prefix: str) -> None:
    capture = os.path.realpath(layout.capture_root)
    marker = Path(layout.capture_root, MARKER_NAME)
    try:
        if marker.read_text(encoding="utf-8") != layout.token:
            raise SecurityAbort("capture root marker does not belong to this run")
    except OSError as exc:
        raise SecurityAbort("capture root marker is missing") from exc
    for path in interpretations:
        if not _within(os.path, os.path.realpath(path), capture):
            raise SecurityAbort("a spelling canonicalizes outside the capture root")
    real = os.path.realpath(target)
    if kind == INSIDE:
        if not Path(target).is_file() or not _within(
                os.path, real, os.path.realpath(layout.route_roots[prefix])):
            raise SecurityAbort("inside-root fixture is not a file inside its declared root")
        return
    if real != os.path.realpath(layout.sentinel):
        raise SecurityAbort("probe target is not the run-owned sentinel")
    try:
        content = Path(layout.sentinel).read_text(encoding="utf-8")
    except OSError as exc:
        raise SecurityAbort("sentinel is missing") from exc
    if content != sentinel_content(layout.token):
        raise SecurityAbort("sentinel content does not belong to this run")


def sentinel_content(token: str) -> str:
    return f"vosu-route-probe-sentinel {token}\n"


def security_target(layout: SecurityLayout, prefix: str, spelling: str) -> tuple[str, bool]:
    """Validate and return (request path, lexical_only). Raises SecurityAbort before any send."""
    if spelling not in SPELLING_BY_NAME:
        raise SecurityAbort(f"unknown spelling {spelling!r}")
    if layout.os_name not in SPELLING_BY_NAME[spelling].oses:
        raise SecurityAbort(f"{spelling} does not apply to {layout.os_name}")
    if prefix not in layout.route_roots:
        raise SecurityAbort(f"no declared root for {prefix}")
    _check_capture_root(layout)
    pm = _pm(layout.os_name)
    kind = SPELLING_BY_NAME[spelling].kind
    suffix, target = _spell(spelling, layout, prefix)
    if not _within(pm, target, layout.capture_root):
        raise SecurityAbort("probe target is outside the capture root")
    root = layout.route_roots[prefix]
    if kind == OUTSIDE and _within(pm, target, root):
        raise SecurityAbort("outside-root probe target is inside its declared root")
    if kind != OUTSIDE and not _within(pm, target, root):
        raise SecurityAbort("inside-root probe target escapes its declared root")
    interpretations = _interpretations(pm, suffix, root)
    for path in interpretations:
        if not _within(pm, path, layout.capture_root):
            raise SecurityAbort("a spelling interpretation leaves the capture root")
    if kind == OUTSIDE and not any(_norm(pm, path) == _norm(pm, target) for path in interpretations):
        raise SecurityAbort("spelling does not name its sentinel")
    lexical_only = layout.os_name != host_os()
    if not lexical_only:
        _host_checks(layout, kind, target, interpretations, prefix)
    return prefix + suffix, lexical_only


def outside_chart(layout: SecurityLayout) -> tuple[str, bool]:
    """Validate the outside-root calculation sentinel chart; returns (absolute path, lexical_only)."""
    _check_capture_root(layout)
    chart = layout.sentinel_chart
    if chart is None or layout.sentinel_chart_sha256 is None:
        raise SecurityAbort("no sentinel chart outside the declared roots")
    lexical_only = layout.os_name != host_os()
    if not lexical_only:
        _host_checks(layout, OUTSIDE, layout.sentinel, [chart], "")
        if os.path.realpath(chart) != os.path.realpath(
                Path(layout.sentinel).with_name(SENTINEL_CHART)):
            raise SecurityAbort("sentinel chart is not the run-owned copy")
        if hashlib.sha256(Path(chart).read_bytes()).hexdigest() != layout.sentinel_chart_sha256:
            raise SecurityAbort("sentinel chart content changed")
    return chart, lexical_only


def inside_absolute(layout: SecurityLayout, value: str) -> tuple[str, bool]:
    """Validate an absolute calculation path inside a declared root; returns (path, lexical_only)."""
    _check_capture_root(layout)
    pm = _pm(layout.os_name)
    if not _absolute(pm, value) or not any(
            _within(pm, value, root) for _, root in layout.declared_roots()):
        raise SecurityAbort("absolute calculation path is not inside a declared root")
    lexical_only = layout.os_name != host_os()
    if not lexical_only:
        real = os.path.realpath(value)
        if not Path(value).is_file() or not any(
                _within(os.path, real, os.path.realpath(root))
                for _, root in layout.declared_roots()):
            raise SecurityAbort("absolute calculation path does not canonicalize inside a root")
    return value, lexical_only


def prepare_security_layout(capture_root: str, route_roots: Mapping[str, str],
                            inside_files: Mapping[str, str],
                            chart_source: str | None = None,
                            other_roots: Mapping[str, str] | None = None) -> SecurityLayout:
    """Create a fresh marker, sentinel(s) and per-root symlinks; never adopts existing files."""
    capture_root = os.path.abspath(capture_root)
    token = secrets.token_hex(16)
    sentinel_dir = os.path.join(capture_root, SENTINEL_DIR)
    sentinel = os.path.join(sentinel_dir, SENTINEL_FILE)
    chart = os.path.join(sentinel_dir, SENTINEL_CHART) if chart_source else None
    layout = SecurityLayout(host_os(), capture_root, token, dict(route_roots),
                            dict(inside_files), sentinel, chart,
                            other_roots=dict(other_roots or {}))
    _check_capture_root(layout)
    if not os.path.isdir(capture_root):
        raise SecurityAbort("capture root does not exist")
    with open(os.path.join(capture_root, MARKER_NAME), "x", encoding="utf-8") as handle:
        handle.write(token)
    os.mkdir(sentinel_dir)
    with open(sentinel, "x", encoding="utf-8") as handle:
        handle.write(sentinel_content(token))
    chart_sha = None
    if chart_source and chart:
        data = Path(chart_source).read_bytes()
        with open(chart, "xb") as handle:
            handle.write(data)
        chart_sha = hashlib.sha256(data).hexdigest()
    links: dict[str, str] = {}
    errors: dict[str, str] = {}
    created: dict[str, str] = {}
    for prefix, root in route_roots.items():
        root_key = os.path.normcase(os.path.realpath(root))
        if root_key in created:
            links[prefix] = created[root_key]
            continue
        try:
            os.symlink(sentinel, os.path.join(root, LINK_NAME))
        except (OSError, NotImplementedError) as exc:
            errors[prefix] = type(exc).__name__
            continue
        links[prefix] = created[root_key] = LINK_NAME
    return dataclasses.replace(layout, sentinel_chart_sha256=chart_sha, links=links,
                               link_errors=errors)


def remove_security_layout(layout: SecurityLayout) -> None:
    """Remove exactly the files prepare_security_layout created."""
    for prefix, link in layout.links.items():
        path = os.path.join(layout.route_roots[prefix], link)
        if os.path.islink(path) and os.path.realpath(path) == os.path.realpath(layout.sentinel):
            os.unlink(path)
    sentinel_dir = os.path.dirname(layout.sentinel)
    marker = Path(layout.capture_root, MARKER_NAME)
    if marker.is_file() and marker.read_text(encoding="utf-8") == layout.token:
        shutil.rmtree(sentinel_dir, ignore_errors=True)
        marker.unlink()


def sentinel_exposed(body: bytes, layout: SecurityLayout) -> bool:
    """True when a response or frame carries this run's sentinel token."""
    return layout.token.encode("ascii") in body
