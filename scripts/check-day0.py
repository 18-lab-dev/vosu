#!/usr/bin/env python3
"""Fail-closed day-0 release gate and candidate-bound evidence verifier.

Enforces docs/v2/catalog-v2.md (Day-0 release check, A8.4) and
docs/testing/verification.md (M8 migration and release verification, A8.4/A8.6):
- Exact four unauthenticated GitHub latest-release checks (no auth, no cache,
  bounded HTTPS requests/JSON):
  - 18-lab-dev/vosu-tools (requires catalog-v2.json asset, non-draft/prerelease)
  - 18-lab-dev/vosulay (requires catalog-v2.json asset, non-draft/prerelease)
  - 18-lab-dev/vosu-overlays (requires catalog-v2.json asset, non-draft/prerelease)
  - 18-lab-dev/vosu-overlay-quickstart (strict SemVer major 2, non-draft/prerelease)
- Strict 5-column parser for tests/fixtures/upstream/DIVERGENCES.md rejecting
  temporary rows, missing/unknown/ambiguous classes, duplicates, and any
  malformed or stray pipe-delimited row.
- Candidate-bound format-1 release evidence manifest verifier (A8.4 + A8.6).
"""

import argparse
from datetime import datetime
import hashlib
import importlib.util
import json
import math
from pathlib import Path
import re
import socket
import sys
import tomllib
from typing import Any, Callable, NamedTuple
import urllib.error
import urllib.parse
import urllib.request


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_LEDGER = ROOT / "tests" / "fixtures" / "upstream" / "DIVERGENCES.md"
DEFAULT_CARGO_MANIFEST = ROOT / "Cargo.toml"

GITHUB_API_HOST = "api.github.com"
DEFAULT_TIMEOUT_SECONDS = 15.0
MAX_TIMEOUT_SECONDS = 60.0
MAX_RESPONSE_BYTES = 2 * 1024 * 1024  # 2 MiB
MAX_EVIDENCE_BYTES = 16 * 1024 * 1024  # 16 MiB
MAX_REDIRECTS = 3

CATALOG_REPOS = ("18-lab-dev/vosu-tools", "18-lab-dev/vosulay", "18-lab-dev/vosu-overlays")
QUICKSTART_REPO = "18-lab-dev/vosu-overlay-quickstart"
ALL_DAY0_REPOS = (*CATALOG_REPOS, QUICKSTART_REPO)
REQUIRED_CATALOG_ASSET = "catalog-v2.json"

LEDGER_HEADER = ("id", "class", "scope", "reason", "closed by")
PERMANENT_LEDGER_CLASSES = frozenset({"security", "contract", "licensing"})
DEFINED_LEDGER_CLASSES = PERMANENT_LEDGER_CLASSES | frozenset({"temporary"})
SEPARATOR_CELL_RE = re.compile(r"^:?-{3,}:?$")

HEX40_RE = re.compile(r"^[0-9a-f]{40}$")
HEX64_RE = re.compile(r"^[0-9a-f]{64}$")
RFC3339_RE = re.compile(r"^\d{4}-\d{2}-\d{2}[Tt ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:[Zz]|[+-]\d{2}:\d{2})$")

NATIVE_TARGETS = ("x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc")
ALL_TARGETS = frozenset({*NATIVE_TARGETS, "any"})
PLATFORM_TO_TARGET = {"linux": "x86_64-unknown-linux-gnu", "windows": "x86_64-pc-windows-msvc"}
REQUIRED_M4B_PAIRS = frozenset({"windows-stable", "windows-lazer", "linux-lazer"})
REQUIRED_PARITY_STATES = frozenset({"menu", "play", "paused", "result"})
REQUIRED_FORGES = frozenset({"github", "gitlab", "codeberg", "gitea_or_forgejo"})
PACKAGE_MANAGERS = frozenset({"deb", "rpm", "arch", "aur", "winget", "scoop"})
RECEIPT_KINDS = frozenset({"own-ci-process", "native-sandbox", "browser", "package", "sdk-publication", "release", "live"})
RECEIPT_RUNTIMES = frozenset({"process", "in-process", "none"})
HOST_SOURCES = frozenset({"github-release", "local-build"})
PACKAGE_KINDS = frozenset({"plugin", "tool", "overlay"})
CATALOG_FILES = frozenset({"catalog-v2.json", "catalog.json"})

SUPERVISOR_PLUGIN_ID = "vosu.vosulay"
REQUIRED_TOOL_PLUGINS = (
    "vosu.tools.lazer-library", "vosu.tools.map-analysis", "vosu.tools.server-pp",
    "vosu.tools.practice", "vosu.tools.replay-renderer", "vosu.tools.skin-studio", "vosu.tools.downloader",
)


def _load_package_release():
    path = Path(__file__).with_name("package-release.py")
    spec = importlib.util.spec_from_file_location("package_release", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


_PACKAGE_RELEASE = _load_package_release()


class HttpResponse(NamedTuple):
    status: int
    body: bytes
    headers: dict[str, str] | None = None


class RepoCheckResult(NamedTuple):
    repository: str
    passed: bool
    tag_name: str | None = None
    release_id: int | None = None
    error: str | None = None


class LedgerRow(NamedTuple):
    entry_id: str
    entry_class: str
    scope: str
    reason: str
    closed_by: str
    line_number: int


class LedgerCheckResult(NamedTuple):
    passed: bool
    sha256: str
    rows: tuple[LedgerRow, ...]
    counts: dict[str, int]
    errors: tuple[str, ...]


class GateResult(NamedTuple):
    passed: bool
    skipped: bool
    reason: str | None
    repos: tuple[RepoCheckResult, ...]
    ledger: LedgerCheckResult | None
    evidence_errors: tuple[str, ...]
    errors: tuple[str, ...]


Fetcher = Callable[..., HttpResponse]


def parse_semver_tag(tag: str, *, require_v_prefix: bool) -> tuple[tuple[int, int, int], str | None]:
    """Parse strict SemVer 2.0.0 without build metadata using package-release grammar."""
    if not isinstance(tag, str) or not tag:
        raise ValueError(f"release tag must be a non-empty string, got {tag!r}")
    if require_v_prefix:
        if not tag.startswith("v"):
            raise ValueError(f"release tag must start with 'v', got {tag!r}")
        raw = tag[1:]
    else:
        raw = tag[1:] if tag.startswith("v") else tag
    core, prerelease = _PACKAGE_RELEASE.parse_version(raw)
    major_s, minor_s, patch_s = core.split(".")
    return (int(major_s), int(minor_s), int(patch_s)), prerelease


def requires_day0_gate(tag: str) -> bool:
    """Return True iff tag is a stable >= 2.0.0 release requiring the day-0 gate."""
    (major, _minor, _patch), prerelease = parse_semver_tag(tag, require_v_prefix=True)
    return prerelease is None and major >= 2


def read_cargo_source_pin(manifest_path: Path = DEFAULT_CARGO_MANIFEST) -> str | None:
    """Read [package.metadata.vosu] source-commit from Cargo.toml if present."""
    if not manifest_path.is_file():
        return None
    with manifest_path.open("rb") as handle:
        data = tomllib.load(handle)
    pin = data.get("package", {}).get("metadata", {}).get("vosu", {}).get("source-commit")
    return pin if isinstance(pin, str) and HEX40_RE.match(pin) else None


def _validate_github_api_url(url: str) -> None:
    parsed = urllib.parse.urlparse(url)
    if parsed.scheme != "https":
        raise ValueError(f"refusing non-HTTPS URL: {url!r}")
    if parsed.hostname != GITHUB_API_HOST:
        raise ValueError(f"refusing non-GitHub-API host {parsed.hostname!r} in {url!r}")
    if parsed.username or parsed.password:
        raise ValueError(f"refusing URL with embedded credentials: {url!r}")


class _HttpsGithubRedirectHandler(urllib.request.HTTPRedirectHandler):
    def __init__(self, max_redirects: int = MAX_REDIRECTS) -> None:
        super().__init__()
        self._remaining = max_redirects

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if self._remaining <= 0:
            raise urllib.error.HTTPError(
                newurl, code, f"exceeded maximum {MAX_REDIRECTS} redirects", headers, fp
            )
        self._remaining -= 1
        resolved = urllib.parse.urljoin(req.full_url, newurl)
        _validate_github_api_url(resolved)
        redirected = super().redirect_request(req, fp, code, msg, headers, resolved)
        if redirected is not None:
            for key in list(redirected.headers):
                if key.lower() == "authorization":
                    del redirected.headers[key]
        return redirected


def default_fetcher(
    url: str,
    *,
    headers: dict[str, str],
    timeout: float = DEFAULT_TIMEOUT_SECONDS,
    max_bytes: int = MAX_RESPONSE_BYTES,
) -> HttpResponse:
    """Execute one bounded, unauthenticated HTTPS GET against api.github.com."""
    _validate_github_api_url(url)
    if any(k.lower() == "authorization" for k in headers):
        raise ValueError("day-0 checks must be strictly unauthenticated (Authorization forbidden)")
    if timeout <= 0 or timeout > MAX_TIMEOUT_SECONDS:
        raise ValueError(f"timeout must be in (0, {MAX_TIMEOUT_SECONDS}], got {timeout}")
    request = urllib.request.Request(url, headers=dict(headers), method="GET")
    opener = urllib.request.build_opener(_HttpsGithubRedirectHandler())
    try:
        with opener.open(request, timeout=timeout) as response:
            body = response.read(max_bytes + 1)
            if len(body) > max_bytes:
                raise ValueError(f"response body exceeded {max_bytes} byte limit")
            resp_headers = {k.lower(): v for k, v in response.headers.items()}
            return HttpResponse(status=int(response.status), body=body, headers=resp_headers)
    except urllib.error.HTTPError as err:
        snippet = err.read(4096) if hasattr(err, "read") else b""
        return HttpResponse(status=int(err.code), body=snippet)


def check_single_repository(
    repository: str,
    *,
    fetcher: Fetcher = default_fetcher,
    timeout: float = DEFAULT_TIMEOUT_SECONDS,
    max_bytes: int = MAX_RESPONSE_BYTES,
) -> RepoCheckResult:
    """Validate one repository's unauthenticated GitHub /releases/latest response."""
    if repository not in ALL_DAY0_REPOS:
        return RepoCheckResult(repository, False, error=f"unexpected repository {repository!r}")
    url = f"https://{GITHUB_API_HOST}/repos/{repository}/releases/latest"
    headers = {"Accept": "application/vnd.github+json", "User-Agent": "vosu-day0-gate"}
    try:
        response = fetcher(url, headers=headers, timeout=timeout, max_bytes=max_bytes)
    except (urllib.error.URLError, TimeoutError, socket.timeout, OSError, ValueError) as exc:
        return RepoCheckResult(repository, False, error=f"{repository}: request failed: {exc}")

    if not hasattr(response, "status") or not hasattr(response, "body") or not _is_int(response.status):
        return RepoCheckResult(repository, False, error=f"{repository}: fetcher returned invalid response object")
    if response.status != 200:
        return RepoCheckResult(repository, False, error=f"{repository}: HTTP {response.status} from {url}")
    if not isinstance(response.body, (bytes, bytearray)):
        return RepoCheckResult(repository, False, error=f"{repository}: non-bytes response body")
    if len(response.body) > max_bytes:
        return RepoCheckResult(
            repository, False,
            error=f"{repository}: response body ({len(response.body)} bytes) exceeds {max_bytes} byte limit",
        )
    try:
        payload = json.loads(response.body.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        return RepoCheckResult(repository, False, error=f"{repository}: malformed JSON response: {exc}")
    if not isinstance(payload, dict):
        return RepoCheckResult(repository, False, error=f"{repository}: expected JSON object payload")

    for flag in ("draft", "prerelease"):
        val = payload.get(flag)
        if not isinstance(val, bool) or val:
            return RepoCheckResult(repository, False, error=f"{repository}: release must have {flag}=false, got {val!r}")

    tag_name = payload.get("tag_name")
    if not _is_nonempty_str(tag_name):
        return RepoCheckResult(repository, False, error=f"{repository}: missing or empty tag_name")
    release_id = payload.get("id")
    if release_id is not None and not _is_int(release_id):
        return RepoCheckResult(repository, False, error=f"{repository}: invalid release id {release_id!r}")

    if repository == QUICKSTART_REPO:
        try:
            (major, _minor, _patch), pre = parse_semver_tag(tag_name, require_v_prefix=False)
        except ValueError as exc:
            return RepoCheckResult(repository, False, tag_name, release_id, f"{repository}: invalid SemVer tag_name {tag_name!r}: {exc}")
        if pre is not None:
            return RepoCheckResult(repository, False, tag_name, release_id, f"{repository}: prerelease tag_name {tag_name!r} rejected")
        if major != 2:
            return RepoCheckResult(repository, False, tag_name, release_id, f"{repository}: tag_name {tag_name!r} has major {major}, expected 2")
        return RepoCheckResult(repository, True, tag_name, release_id)

    assets = payload.get("assets")
    if not isinstance(assets, list):
        return RepoCheckResult(repository, False, tag_name, release_id, f"{repository}: missing or non-list assets field")
    asset_names: set[str] = set()
    for index, asset in enumerate(assets):
        if not isinstance(asset, dict) or not isinstance(asset.get("name"), str):
            return RepoCheckResult(repository, False, tag_name, release_id, f"{repository}: malformed asset entry at index {index}")
        asset_names.add(asset["name"])
    if REQUIRED_CATALOG_ASSET not in asset_names:
        return RepoCheckResult(
            repository, False, tag_name, release_id,
            f"{repository}: latest release {tag_name!r} is missing required asset {REQUIRED_CATALOG_ASSET!r}",
        )
    return RepoCheckResult(repository, True, tag_name, release_id)


def check_all_repositories(
    *,
    fetcher: Fetcher = default_fetcher,
    timeout: float = DEFAULT_TIMEOUT_SECONDS,
    max_bytes: int = MAX_RESPONSE_BYTES,
) -> tuple[RepoCheckResult, ...]:
    """Check all four day-0 repositories without short-circuiting."""
    return tuple(
        check_single_repository(repo, fetcher=fetcher, timeout=timeout, max_bytes=max_bytes)
        for repo in ALL_DAY0_REPOS
    )


def _split_pipe_row(stripped: str) -> list[str] | None:
    if not (stripped.startswith("|") and stripped.endswith("|")) or len(stripped) < 2:
        return None
    return [cell.strip() for cell in stripped[1:-1].split("|")]


def validate_ledger_text(text: str, *, raw_bytes: bytes | None = None) -> LedgerCheckResult:
    """Strictly parse and validate tests/fixtures/upstream/DIVERGENCES.md."""
    digest = hashlib.sha256(raw_bytes if raw_bytes is not None else text.encode("utf-8")).hexdigest()
    lines = text.splitlines()
    errors: list[str] = []
    rows: list[LedgerRow] = []
    counts = {"security": 0, "contract": 0, "licensing": 0, "temporary": 0}
    seen_ids: set[str] = set()

    header_indices = [
        idx for idx, line in enumerate(lines)
        if (cells := _split_pipe_row(line.strip())) is not None and tuple(cells) == LEDGER_HEADER
    ]
    if len(header_indices) != 1:
        msg = (
            "divergence ledger is missing required header '| id | class | scope | reason | closed by |'"
            if not header_indices
            else f"divergence ledger has multiple header tables (lines {', '.join(str(i + 1) for i in header_indices)})"
        )
        return LedgerCheckResult(False, digest, (), counts, (msg,))

    header_idx = header_indices[0]
    sep_idx = header_idx + 1
    if sep_idx >= len(lines):
        return LedgerCheckResult(False, digest, (), counts, (f"line {header_idx + 1}: missing separator row",))
    sep_cells = _split_pipe_row(lines[sep_idx].strip())
    if sep_cells is None or len(sep_cells) != 5 or not all(SEPARATOR_CELL_RE.match(c) for c in sep_cells):
        return LedgerCheckResult(
            False, digest, (), counts, (f"line {sep_idx + 1}: malformed separator row {lines[sep_idx]!r}",)
        )

    for idx in range(header_idx):
        if lines[idx].strip().startswith("|"):
            errors.append(f"line {idx + 1}: stray pipe-delimited row outside the divergence ledger table")

    idx = sep_idx + 1
    while idx < len(lines):
        stripped = lines[idx].strip()
        if not stripped:
            idx += 1
            break
        cells = _split_pipe_row(stripped)
        if cells is None:
            errors.append(f"line {idx + 1}: malformed ledger row inside table: {stripped!r}")
            idx += 1
            continue
        if len(cells) != 5:
            errors.append(f"line {idx + 1}: malformed ledger row has {len(cells)} cells, expected 5: {stripped!r}")
            idx += 1
            continue
        empty_fields = [name for name, val in zip(LEDGER_HEADER, cells) if not val]
        if empty_fields:
            errors.append(f"line {idx + 1}: ledger row has empty cell(s) {', '.join(empty_fields)}")
            idx += 1
            continue
        entry_id, entry_class, scope, reason, closed_by = cells
        if entry_id in seen_ids:
            errors.append(f"line {idx + 1}: duplicate ledger id {entry_id!r}")
        else:
            seen_ids.add(entry_id)
        if entry_class == "temporary":
            counts["temporary"] += 1
            errors.append(f"line {idx + 1}: temporary divergence {entry_id!r} is forbidden at day-0")
        elif entry_class in PERMANENT_LEDGER_CLASSES:
            counts[entry_class] += 1
        else:
            errors.append(f"line {idx + 1}: ledger row {entry_id!r} has invalid/ambiguous class {entry_class!r}")
        rows.append(LedgerRow(entry_id, entry_class, scope, reason, closed_by, idx + 1))
        idx += 1

    while idx < len(lines):
        if lines[idx].strip().startswith("|"):
            errors.append(f"line {idx + 1}: stray pipe-delimited row outside the divergence ledger table")
        idx += 1

    if not rows:
        errors.append("divergence ledger table has no entries")
    return LedgerCheckResult(not errors, digest, tuple(rows), counts, tuple(errors))


def validate_ledger_file(path: Path = DEFAULT_LEDGER) -> LedgerCheckResult:
    """Read and strictly validate the divergence ledger file."""
    empty_counts = {"security": 0, "contract": 0, "licensing": 0, "temporary": 0}
    try:
        raw = path.read_bytes()
    except OSError as exc:
        return LedgerCheckResult(False, "", (), empty_counts, (f"cannot read divergence ledger {path}: {exc}",))
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        return LedgerCheckResult(
            False, hashlib.sha256(raw).hexdigest(), (), empty_counts,
            (f"divergence ledger {path} is not valid UTF-8: {exc}",),
        )
    return validate_ledger_text(text, raw_bytes=raw)


def _is_nonempty_str(value: Any) -> bool:
    return isinstance(value, str) and bool(value.strip())


def _is_hex40(value: Any) -> bool:
    return isinstance(value, str) and bool(HEX40_RE.match(value))


def _is_hex64(value: Any) -> bool:
    return isinstance(value, str) and bool(HEX64_RE.match(value))


def _is_rfc3339(value: Any) -> bool:
    if not isinstance(value, str) or not RFC3339_RE.match(value):
        return False
    try:
        datetime.fromisoformat(value[:-1] + "+00:00" if value.endswith(("Z", "z")) else value)
        return True
    except ValueError:
        return False


def _is_int(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def _is_finite_nonneg(value: Any) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value) and value >= 0


def _chk(cond: bool, msg: str, errors: list[str]) -> bool:
    if not cond:
        errors.append(msg)
    return cond


def _validate_ecosystem_receipts(
    receipts: Any, *, day0_repo_commits: dict[str, str], candidate_commit: str | None, errors: list[str]
) -> None:
    if not _chk(isinstance(receipts, list) and bool(receipts), "ecosystem_receipts must be a non-empty list", errors):
        return
    process_host_tags: set[str] = set()
    tool_process_coverage: set[tuple[str, str]] = set()
    supervisor_process_targets: set[str] = set()
    supervisor_sandbox_targets: set[str] = set()

    for idx, r in enumerate(receipts):
        ctx = f"ecosystem_receipts[{idx}]"
        if not _chk(isinstance(r, dict), f"{ctx} must be an object", errors):
            continue
        _chk(_is_int(r.get("receipt_version")) and r.get("receipt_version") == 1, f"{ctx}.receipt_version must be 1", errors)
        kind, repo, commit = r.get("kind"), r.get("repository"), r.get("commit")
        _chk(kind in RECEIPT_KINDS, f"{ctx}.kind invalid: {kind!r}", errors)
        _chk(_is_nonempty_str(repo) and repo.startswith("18-lab-dev/"), f"{ctx}.repository must be 18-lab-dev/<repo>", errors)
        if _chk(_is_hex40(commit), f"{ctx}.commit must be 40-hex", errors):
            if repo in day0_repo_commits:
                _chk(commit == day0_repo_commits[repo], f"{ctx}.commit does not match day0_check commit for {repo}", errors)
            elif repo == "18-lab-dev/vosu" and candidate_commit:
                _chk(commit == candidate_commit, f"{ctx}.commit does not match candidate.commit", errors)
        _chk(r.get("tag") is None or _is_nonempty_str(r.get("tag")), f"{ctx}.tag must be null or string", errors)

        host = r.get("host")
        if _chk(isinstance(host, dict), f"{ctx}.host must be an object", errors):
            host_tag = host.get("tag")
            try:
                parse_semver_tag(host_tag, require_v_prefix=True)
                if kind == "own-ci-process":
                    process_host_tags.add(host_tag)
            except ValueError:
                errors.append(f"{ctx}.host.tag must be a valid SemVer v-tag, got {host_tag!r}")
            _chk(_is_hex64(host.get("binary_sha256")), f"{ctx}.host.binary_sha256 must be 64-hex", errors)
            _chk(host.get("source") in HOST_SOURCES, f"{ctx}.host.source invalid", errors)

        sdk = r.get("sdk")
        if sdk is not None and _chk(isinstance(sdk, dict), f"{ctx}.sdk must be an object or null", errors):
            _chk(_is_nonempty_str(sdk.get("version")), f"{ctx}.sdk.version required", errors)
            _chk(_is_hex64(sdk.get("crate_sha256")), f"{ctx}.sdk.crate_sha256 must be 64-hex", errors)

        target, runtime, child = r.get("target"), r.get("runtime"), r.get("child")
        _chk(target in ALL_TARGETS, f"{ctx}.target invalid: {target!r}", errors)
        _chk(runtime in RECEIPT_RUNTIMES, f"{ctx}.runtime invalid: {runtime!r}", errors)
        is_fixture = isinstance(child, str) and child.startswith("fixture:") and bool(child[8:].strip())
        _chk(child is None or child == "production" or is_fixture, f"{ctx}.child invalid: {child!r}", errors)

        package_ids: set[str] = set()
        if _chk(isinstance(r.get("packages"), list), f"{ctx}.packages must be a list", errors):
            for p_idx, pkg in enumerate(r["packages"]):
                p_ctx = f"{ctx}.packages[{p_idx}]"
                if not _chk(isinstance(pkg, dict), f"{p_ctx} must be an object", errors):
                    continue
                if _chk(_is_nonempty_str(pkg.get("id")), f"{p_ctx}.id required", errors):
                    package_ids.add(pkg["id"])
                _chk(pkg.get("kind") in PACKAGE_KINDS, f"{p_ctx}.kind invalid", errors)
                _chk(_is_nonempty_str(pkg.get("version")) and _is_nonempty_str(pkg.get("file")), f"{p_ctx} version/file required", errors)
                _chk(pkg.get("target") in ALL_TARGETS, f"{p_ctx}.target invalid", errors)
                _chk(_is_hex64(pkg.get("sha256")) and _is_int(pkg.get("size")) and pkg["size"] > 0, f"{p_ctx} sha256/size invalid", errors)

        if _chk(isinstance(r.get("catalogs"), list), f"{ctx}.catalogs must be a list", errors):
            for c_idx, cat in enumerate(r["catalogs"]):
                c_ctx = f"{ctx}.catalogs[{c_idx}]"
                if _chk(isinstance(cat, dict), f"{c_ctx} must be an object", errors):
                    _chk(cat.get("file") in CATALOG_FILES and _is_hex64(cat.get("sha256")) and _is_int(cat.get("size")) and cat["size"] > 0, f"{c_ctx} invalid", errors)

        wf = r.get("workflow")
        if _chk(isinstance(wf, dict), f"{ctx}.workflow must be an object", errors):
            _chk(_is_nonempty_str(wf.get("run_url")) and wf["run_url"].startswith("https://"), f"{ctx}.workflow.run_url must be https://", errors)
            _chk(_is_int(wf.get("run_id")) and wf["run_id"] > 0 and _is_int(wf.get("attempt")) and wf["attempt"] > 0 and _is_nonempty_str(wf.get("job")), f"{ctx}.workflow fields invalid", errors)

        tests_ok = _chk(isinstance(r.get("tests"), list) and bool(r.get("tests")), f"{ctx}.tests must be non-empty (cannot infer pass from an asset name)", errors)
        if tests_ok:
            for t_idx, t in enumerate(r["tests"]):
                t_ctx = f"{ctx}.tests[{t_idx}]"
                if not _chk(isinstance(t, dict), f"{t_ctx} must be an object", errors):
                    tests_ok = False
                    continue
                sel, pas, fail, skip = t.get("selected"), t.get("passed"), t.get("failed"), t.get("skipped")
                t_valid = (
                    _chk(_is_nonempty_str(t.get("suite")) and _is_nonempty_str(t.get("command")), f"{t_ctx} suite/command required", errors)
                    and _chk(all(_is_int(v) and v >= 0 for v in (sel, pas, fail, skip)), f"{t_ctx} counts must be >= 0 ints", errors)
                )
                if t_valid:
                    t_valid = (
                        _chk(sel > 0 and pas > 0 and fail == 0, f"{t_ctx} must have selected>0, passed>0, failed==0", errors)
                        and _chk(pas + fail + skip == sel, f"{t_ctx}: passed+failed+skipped != selected", errors)
                    )
                tests_ok = tests_ok and t_valid

        nat = r.get("native")
        _chk(isinstance(nat, dict) and isinstance(nat.get("sandbox"), dict) and isinstance(nat.get("cleanup"), list), f"{ctx}.native invalid", errors)
        _chk(isinstance(r.get("approvals"), list) and all(_is_nonempty_str(a) for a in r["approvals"]), f"{ctx}.approvals invalid", errors)
        _chk(r.get("result") == "pass", f"{ctx}.result must be 'pass', got {r.get('result')!r}", errors)
        _chk(_is_rfc3339(r.get("started_at")) and _is_rfc3339(r.get("finished_at")), f"{ctx} timestamps must be RFC 3339", errors)

        passed_receipt = tests_ok and r.get("result") == "pass" and target in NATIVE_TARGETS
        if kind == "own-ci-process":
            _chk(runtime == "process", f"{ctx}: own-ci-process must have runtime='process', got {runtime!r}", errors)
            _chk(target in NATIVE_TARGETS and bool(package_ids), f"{ctx}: own-ci-process requires native target and packages", errors)
            if SUPERVISOR_PLUGIN_ID in package_ids:
                if _chk(is_fixture, f"{ctx}: A8.6 requires {SUPERVISOR_PLUGIN_ID} own-ci-process child='fixture:<id>'", errors) and passed_receipt and runtime == "process":
                    supervisor_process_targets.add(target)
            tools = package_ids & set(REQUIRED_TOOL_PLUGINS)
            if tools and _chk(child == "production", f"{ctx}: official plugin own-ci-process requires child='production'", errors) and passed_receipt and runtime == "process":
                tool_process_coverage.update((pid, target) for pid in tools)

        if kind == "native-sandbox":
            _chk(target in NATIVE_TARGETS, f"{ctx}: native-sandbox must target native OS, got {target!r}", errors)
            if SUPERVISOR_PLUGIN_ID in package_ids:
                ok_rt = _chk(runtime == "in-process", f"{ctx}: A8.6 requires {SUPERVISOR_PLUGIN_ID} native-sandbox runtime='in-process'", errors)
                ok_ch = _chk(child == "production", f"{ctx}: A8.6 forbids fixture child as production CEF evidence", errors)
                if passed_receipt and ok_rt and ok_ch:
                    supervisor_sandbox_targets.add(target)

    _chk(len(process_host_tags) <= 1, f"ecosystem_receipts mix inconsistent own-ci-process host tags: {sorted(process_host_tags)}", errors)
    for target in NATIVE_TARGETS:
        _chk(target in supervisor_process_targets, f"A8.6 incomplete: missing {SUPERVISOR_PLUGIN_ID} own-ci-process (fixture) for {target}", errors)
        _chk(target in supervisor_sandbox_targets, f"A8.6 incomplete: missing {SUPERVISOR_PLUGIN_ID} native-sandbox (in-process production) for {target}", errors)
        for pid in REQUIRED_TOOL_PLUGINS:
            _chk((pid, target) in tool_process_coverage, f"missing own-ci-process receipt for {pid} on {target}", errors)


def _validate_native_verification(
    nv: Any, *, cand_commit: str | None, artifact_hashes_by_target: dict[str, set[str]], errors: list[str]
) -> None:
    if not _chk(isinstance(nv, dict), "native_verification must be an object", errors):
        return
    for field in ("linux_prepush_sha", "windows_native_sha"):
        sha = nv.get(field)
        if _chk(_is_hex40(sha), f"native_verification.{field} must be 40-hex", errors) and cand_commit:
            _chk(sha == cand_commit, f"native_verification.{field} does not match candidate.commit", errors)

    def _check_platform_list(key: str, allowed: set[str] | frozenset[str], item_fn: Callable[[str, dict], None]) -> None:
        items = nv.get(key)
        if not _chk(isinstance(items, list), f"native_verification.{key} must be a list", errors):
            return
        seen: set[str] = set()
        plat_key = "forge" if key == "d15_forge_receipts" else "platform"
        for idx, item in enumerate(items):
            ctx = f"native_verification.{key}[{idx}]"
            if not _chk(isinstance(item, dict), f"{ctx} must be an object", errors):
                continue
            plat = item.get(plat_key)
            if _chk(plat in allowed and plat not in seen, f"{ctx}.{plat_key} invalid/duplicate: {plat!r}", errors):
                seen.add(plat)
            item_fn(ctx, item)
            _chk(item.get("result") == "pass", f"{ctx}.result must be 'pass'", errors)
        missing = sorted(set(allowed) - seen)
        _chk(not missing, f"native_verification.{key} missing: {', '.join(missing)}", errors)

    def _smoke_item(ctx: str, item: dict) -> None:
        plat, b_sha = item.get("platform"), item.get("binary_sha256")
        if _chk(_is_hex64(b_sha), f"{ctx}.binary_sha256 must be 64-hex", errors) and plat in PLATFORM_TO_TARGET:
            _chk(b_sha in artifact_hashes_by_target[PLATFORM_TO_TARGET[plat]], f"{ctx}.binary_sha256 does not match any candidate artifact", errors)
        _chk(_is_hex64(item.get("child_fixture_sha256")), f"{ctx}.child_fixture_sha256 must be 64-hex", errors)
        for flag in ("api_ingame_404", "consoleless_lifecycle_stop", "no_orphans"):
            _chk(item.get(flag) is True, f"{ctx}.{flag} must be true", errors)

    def _updater_item(ctx: str, item: dict) -> None:
        plat, c_sha = item.get("platform"), item.get("candidate_sha256")
        _chk(_is_hex64(item.get("source_binary_sha256")), f"{ctx}.source_binary_sha256 must be 64-hex", errors)
        if _chk(_is_hex64(c_sha), f"{ctx}.candidate_sha256 must be 64-hex", errors) and plat in PLATFORM_TO_TARGET:
            _chk(c_sha in artifact_hashes_by_target[PLATFORM_TO_TARGET[plat]], f"{ctx}.candidate_sha256 does not match any candidate artifact", errors)
        for flag in ("decline_no_download", "accept_upgrade", "rollback"):
            _chk(item.get(flag) is True, f"{ctx}.{flag} must be true", errors)

    def _session_item(ctx: str, item: dict) -> None:
        _chk(_is_nonempty_str(item.get("client_version")), f"{ctx}.client_version required", errors)
        st = item.get("states_traversed")
        _chk(isinstance(st, list) and len(st) == 4 and set(st) == REQUIRED_PARITY_STATES, f"{ctx}.states_traversed incomplete", errors)
        ids = item.get("installed_ids")
        if _chk(isinstance(ids, list) and bool(ids) and all(_is_nonempty_str(x) for x in ids), f"{ctx}.installed_ids required", errors):
            _chk("vosulay-editor" in ids and SUPERVISOR_PLUGIN_ID in ids, f"{ctx}.installed_ids missing vosulay-editor/{SUPERVISOR_PLUGIN_ID}", errors)
        _chk(item.get("cef_sandbox_verified") is True and item.get("cleanup_verified") is True, f"{ctx} cef_sandbox_verified/cleanup_verified must be true", errors)

    _check_platform_list("packaged_smoke", set(PLATFORM_TO_TARGET), _smoke_item)
    _check_platform_list("updater_1_2_4", set(PLATFORM_TO_TARGET), _updater_item)
    _check_platform_list("owner_live_sessions", REQUIRED_M4B_PAIRS, _session_item)
    _check_platform_list("d15_forge_receipts", REQUIRED_FORGES, lambda ctx, item: _chk(_is_rfc3339(item.get("checked_at")), f"{ctx}.checked_at must be RFC 3339", errors))

    pkgs = nv.get("packages")
    if _chk(isinstance(pkgs, list) and bool(pkgs), "native_verification.packages must be non-empty", errors):
        for idx, item in enumerate(pkgs):
            ctx = f"native_verification.packages[{idx}]"
            if not _chk(isinstance(item, dict), f"{ctx} must be an object", errors):
                continue
            _chk(item.get("manager") in PACKAGE_MANAGERS and _is_nonempty_str(item.get("owner_repo")) and _is_nonempty_str(item.get("package_id")), f"{ctx} manager/owner_repo/package_id invalid", errors)
            _chk(_is_hex64(item.get("artifact_sha256")) and item.get("install_upgrade_uninstall") is True and item.get("result") == "pass", f"{ctx} artifact_sha256/install_upgrade_uninstall/result invalid", errors)


def validate_evidence_manifest(
    manifest: Any,
    *,
    expected_tag: str | None = None,
    expected_commit: str | None = None,
    expected_source_pin: str | None = None,
    live_ledger: LedgerCheckResult | None = None,
    live_repos: tuple[RepoCheckResult, ...] | None = None,
    expected_release_assets: dict[str, str] | None = None,
) -> tuple[str, ...]:
    """Validate a candidate-bound format-1 release evidence manifest (A8.4 + A8.6)."""
    if not isinstance(manifest, dict):
        return ("evidence manifest root must be a JSON object",)
    errors: list[str] = []
    _chk(_is_int(manifest.get("manifest_version")) and manifest.get("manifest_version") == 1, "manifest_version must be integer 1", errors)

    # 1. candidate
    cand = manifest.get("candidate")
    cand_commit: str | None = None
    if _chk(isinstance(cand, dict), "candidate must be an object", errors):
        cand_tag = cand.get("tag")
        try:
            _chk(requires_day0_gate(cand_tag), f"candidate.tag {cand_tag!r} must be a stable >= 2.0.0 tag", errors)
        except ValueError as exc:
            errors.append(f"candidate.tag invalid: {exc}")
        else:
            _chk(cand.get("version") == cand_tag[1:], "candidate.version does not match candidate.tag", errors)
            if expected_tag is not None:
                _chk(cand_tag == expected_tag, f"candidate.tag {cand_tag!r} != expected {expected_tag!r}", errors)
        if _chk(_is_hex40(cand.get("commit")), "candidate.commit must be 40-hex", errors):
            cand_commit = cand["commit"]
            if expected_commit is not None:
                _chk(cand_commit == expected_commit, f"candidate.commit {cand_commit!r} != expected {expected_commit!r}", errors)
        _chk(_is_rfc3339(cand.get("created_at")), "candidate.created_at must be RFC 3339", errors)
        sdk = cand.get("sdk")
        if _chk(isinstance(sdk, dict), "candidate.sdk must be an object", errors):
            try:
                _PACKAGE_RELEASE.parse_version(sdk.get("version", ""))
            except (TypeError, ValueError):
                errors.append(f"candidate.sdk.version must be valid SemVer, got {sdk.get('version')!r}")
            _chk(_is_hex64(sdk.get("crate_sha256")), "candidate.sdk.crate_sha256 must be 64-hex", errors)
            _chk(_is_hex64(sdk.get("header_sha256")), "candidate.sdk.header_sha256 must be 64-hex", errors)

    # 2. artifacts
    artifact_hashes_by_target: dict[str, set[str]] = {t: set() for t in ALL_TARGETS}
    artifact_by_file: dict[str, str] = {}
    artifacts = manifest.get("artifacts")
    if _chk(isinstance(artifacts, list) and bool(artifacts), "artifacts must be a non-empty list", errors):
        for idx, art in enumerate(artifacts):
            ctx = f"artifacts[{idx}]"
            if not _chk(isinstance(art, dict), f"{ctx} must be an object", errors):
                continue
            name, target, sha = art.get("file"), art.get("target"), art.get("sha256")
            ok_name = _chk(_is_nonempty_str(name), f"{ctx}.file required", errors)
            if ok_name:
                _chk(name not in artifact_by_file, f"{ctx}.file duplicate {name!r}", errors)
            ok_target = _chk(target in ALL_TARGETS, f"{ctx}.target invalid", errors)
            ok_sha = _chk(_is_hex64(sha), f"{ctx}.sha256 must be 64-hex", errors)
            _chk(_is_int(art.get("size")) and art["size"] > 0, f"{ctx}.size must be > 0", errors)
            _chk(isinstance(art.get("signed"), bool), f"{ctx}.signed must be boolean", errors)
            if ok_name and ok_target and ok_sha:
                artifact_by_file[name] = sha
                artifact_hashes_by_target[target].add(sha)
        for target in NATIVE_TARGETS:
            _chk(bool(artifact_hashes_by_target[target]), f"artifacts missing entry for {target}", errors)
        if expected_release_assets:
            for asset_name, asset_sha in sorted(expected_release_assets.items()):
                rec = artifact_by_file.get(asset_name)
                if rec is None:
                    errors.append(f"release asset {asset_name!r} is missing from evidence manifest artifacts")
                elif rec != asset_sha:
                    errors.append(f"release asset {asset_name!r} SHA-256 {asset_sha} differs from evidence manifest {rec}")

    # 3. day0_check
    day0 = manifest.get("day0_check")
    day0_repo_commits: dict[str, str] = {}
    if _chk(isinstance(day0, dict), "day0_check must be an object", errors):
        _chk(_is_rfc3339(day0.get("checked_at")), "day0_check.checked_at must be RFC 3339", errors)
        l_sha = day0.get("ledger_sha256")
        if _chk(_is_hex64(l_sha), "day0_check.ledger_sha256 must be 64-hex", errors) and live_ledger and live_ledger.passed:
            _chk(l_sha == live_ledger.sha256, "day0_check.ledger_sha256 does not match current DIVERGENCES.md", errors)
        lc = day0.get("ledger_counts")
        if _chk(isinstance(lc, dict), "day0_check.ledger_counts must be an object", errors):
            valid_counts = all(_chk(_is_int(lc.get(k)) and lc[k] >= 0, f"day0_check.ledger_counts.{k} must be >= 0 int", errors) for k in ("security", "contract", "licensing", "temporary"))
            _chk(lc.get("temporary") == 0, "day0_check.ledger_counts.temporary must be 0", errors)
            if valid_counts and live_ledger and live_ledger.passed:
                _chk({k: lc[k] for k in live_ledger.counts} == live_ledger.counts, "day0_check.ledger_counts does not match DIVERGENCES.md", errors)
        repos = day0.get("repositories")
        live_by_repo = {r.repository: r for r in live_repos} if live_repos is not None else {}
        if _chk(isinstance(repos, list), "day0_check.repositories must be a list", errors):
            seen_repos: set[str] = set()
            for idx, entry in enumerate(repos):
                ctx = f"day0_check.repositories[{idx}]"
                if not _chk(isinstance(entry, dict), f"{ctx} must be an object", errors):
                    continue
                rn = entry.get("repository")
                if not _chk(rn in ALL_DAY0_REPOS and rn not in seen_repos, f"{ctx}.repository invalid or duplicate: {rn!r}", errors):
                    continue
                seen_repos.add(rn)
                rt, rc, rid = entry.get("tag"), entry.get("commit"), entry.get("release_id")
                _chk(_is_nonempty_str(rt), f"{ctx}.tag required", errors)
                if _chk(_is_hex40(rc), f"{ctx}.commit must be 40-hex", errors):
                    day0_repo_commits[rn] = rc
                _chk(_is_int(rid) and rid > 0, f"{ctx}.release_id must be > 0", errors)
                _chk(_is_rfc3339(entry.get("checked_at")), f"{ctx}.checked_at must be RFC 3339", errors)
                if rn == QUICKSTART_REPO:
                    try:
                        (maj, _mi, _pa), pre = parse_semver_tag(rt, require_v_prefix=False)
                        _chk(pre is None and maj == 2, f"{ctx}.tag {rt!r} must be stable SemVer major 2", errors)
                    except ValueError as exc:
                        errors.append(f"{ctx}.tag invalid SemVer: {exc}")
                    _chk(entry.get("catalog_v2_sha256") is None, f"{ctx}.catalog_v2_sha256 must be null for quickstart", errors)
                    _chk(entry.get("catalog_v1_sha256") is None, f"{ctx}.catalog_v1_sha256 must be null for quickstart", errors)
                else:
                    _chk(_is_hex64(entry.get("catalog_v2_sha256")), f"{ctx}.catalog_v2_sha256 must be 64-hex", errors)
                    _chk(_is_hex64(entry.get("catalog_v1_sha256")), f"{ctx}.catalog_v1_sha256 must be 64-hex", errors)
                live = live_by_repo.get(rn)
                if live and live.passed:
                    if live.tag_name is not None:
                        _chk(rt == live.tag_name, f"{ctx}.tag {rt!r} is stale; live latest is {live.tag_name!r}", errors)
                    if live.release_id is not None:
                        _chk(rid == live.release_id, f"{ctx}.release_id {rid!r} is stale; live is {live.release_id!r}", errors)
            missing_repos = sorted(set(ALL_DAY0_REPOS) - seen_repos)
            _chk(not missing_repos, f"day0_check.repositories missing: {', '.join(missing_repos)}", errors)

    # 4. ecosystem_receipts (A8.4 + A8.6)
    _validate_ecosystem_receipts(
        manifest.get("ecosystem_receipts"),
        day0_repo_commits=day0_repo_commits,
        candidate_commit=cand_commit,
        errors=errors,
    )

    # 5. m4b_parity
    m4b = manifest.get("m4b_parity")
    if _chk(isinstance(m4b, dict), "m4b_parity must be an object", errors):
        pin = m4b.get("source_pin")
        if _chk(_is_hex40(pin), "m4b_parity.source_pin must be 40-hex", errors) and expected_source_pin is not None:
            _chk(pin == expected_source_pin, f"m4b_parity.source_pin {pin!r} != Cargo pin {expected_source_pin!r}", errors)
        _chk(_is_hex64(m4b.get("inventory_sha256")), "m4b_parity.inventory_sha256 must be 64-hex", errors)
        pairs = m4b.get("pairs")
        if _chk(isinstance(pairs, list), "m4b_parity.pairs must be a list", errors):
            seen_pairs: set[str] = set()
            for idx, p in enumerate(pairs):
                ctx = f"m4b_parity.pairs[{idx}]"
                if not _chk(isinstance(p, dict), f"{ctx} must be an object", errors):
                    continue
                plat = p.get("platform")
                if _chk(plat in REQUIRED_M4B_PAIRS and plat not in seen_pairs, f"{ctx}.platform invalid/duplicate: {plat!r}", errors):
                    seen_pairs.add(plat)
                _chk(_is_nonempty_str(p.get("authorization_ref")), f"{ctx}.authorization_ref required", errors)
                if _chk(_is_hex40(p.get("vosu_commit")), f"{ctx}.vosu_commit must be 40-hex", errors) and cand_commit:
                    _chk(p["vosu_commit"] == cand_commit, f"{ctx}.vosu_commit != candidate.commit", errors)
                _chk(_is_nonempty_str(p.get("client_version")), f"{ctx}.client_version required", errors)
                st = p.get("states")
                _chk(isinstance(st, list) and len(st) == 4 and set(st) == REQUIRED_PARITY_STATES, f"{ctx}.states must cover all 4 states", errors)
                _chk(_is_int(p.get("case_count")) and p["case_count"] > 0, f"{ctx}.case_count must be > 0", errors)
                _chk(_is_hex64(p.get("capture_sha256")), f"{ctx}.capture_sha256 must be 64-hex", errors)
                _chk(p.get("result") == "pass", f"{ctx}.result must be 'pass'", errors)
            missing_pairs = sorted(REQUIRED_M4B_PAIRS - seen_pairs)
            _chk(not missing_pairs, f"m4b_parity.pairs missing: {', '.join(missing_pairs)}", errors)

    # 6. d17_performance
    d17 = manifest.get("d17_performance")
    if _chk(isinstance(d17, dict), "d17_performance must be an object", errors):
        for field in ("decision", "cumulative_verdict", "build_budget_verdict"):
            _chk(d17.get(field) == "pass", f"d17_performance.{field} must be 'pass'", errors)
        for field in ("baseline_pre_m0_commit", "baseline_prev_commit", "candidate_commit"):
            _chk(_is_hex40(d17.get(field)), f"d17_performance.{field} must be 40-hex", errors)
        if cand_commit and _is_hex40(d17.get("candidate_commit")):
            _chk(d17["candidate_commit"] == cand_commit, "d17_performance.candidate_commit != candidate.commit", errors)
        for field in ("linux_receipt_dir", "windows_receipt_dir"):
            _chk(_is_nonempty_str(d17.get(field)), f"d17_performance.{field} required", errors)
        for field in ("broker_linux_ratios", "broker_windows_ratios"):
            ratios = d17.get(field)
            if _chk(isinstance(ratios, dict), f"d17_performance.{field} must be an object", errors):
                pr, rg, cap = ratios.get("precise"), ratios.get("regular"), ratios.get("cap")
                if _chk(all(_is_finite_nonneg(v) for v in (pr, rg, cap)) and cap > 0, f"d17_performance.{field} invalid numbers", errors):
                    _chk(pr <= cap, f"d17_performance.{field}.precise exceeds cap", errors)
                    _chk(rg <= cap, f"d17_performance.{field}.regular exceeds cap", errors)

    # 7. native_verification
    _validate_native_verification(
        manifest.get("native_verification"),
        cand_commit=cand_commit,
        artifact_hashes_by_target=artifact_hashes_by_target,
        errors=errors,
    )
    return tuple(errors)


def validate_evidence_file(
    path: Path,
    *,
    expected_tag: str | None = None,
    expected_commit: str | None = None,
    expected_source_pin: str | None = None,
    live_ledger: LedgerCheckResult | None = None,
    live_repos: tuple[RepoCheckResult, ...] | None = None,
    expected_release_assets: dict[str, str] | None = None,
    max_bytes: int = MAX_EVIDENCE_BYTES,
) -> tuple[str, ...]:
    """Read and validate a candidate-bound release evidence manifest file."""
    try:
        size = path.stat().st_size
        if size > max_bytes:
            return (f"evidence manifest {path} ({size} bytes) exceeds {max_bytes} byte limit",)
        raw = path.read_bytes()
    except OSError as exc:
        return (f"cannot read evidence manifest {path}: {exc}",)
    if len(raw) > max_bytes:
        return (f"evidence manifest {path} ({len(raw)} bytes) exceeds {max_bytes} byte limit",)
    try:
        data = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        return (f"evidence manifest {path} is not valid UTF-8 JSON: {exc}",)
    return validate_evidence_manifest(
        data, expected_tag=expected_tag, expected_commit=expected_commit,
        expected_source_pin=expected_source_pin, live_ledger=live_ledger,
        live_repos=live_repos, expected_release_assets=expected_release_assets,
    )


def run_day0_gate(
    *,
    tag: str | None = None,
    commit: str | None = None,
    evidence_path: Path | None = None,
    require_evidence: bool = False,
    ledger_path: Path = DEFAULT_LEDGER,
    cargo_manifest_path: Path = DEFAULT_CARGO_MANIFEST,
    source_pin: str | None = None,
    expected_release_assets: dict[str, str] | None = None,
    ledger_only: bool = False,
    fetcher: Fetcher = default_fetcher,
    timeout: float = DEFAULT_TIMEOUT_SECONDS,
    max_bytes: int = MAX_RESPONSE_BYTES,
) -> GateResult:
    """Execute the full fail-closed day-0 gate."""
    if ledger_only:
        if tag is not None or require_evidence or evidence_path is not None:
            return GateResult(False, False, None, (), None, (), ("--ledger-only cannot be combined with --tag, --evidence, or --require-evidence",))
        ledger = validate_ledger_file(ledger_path)
        return GateResult(ledger.passed, False, None, (), ledger, (), ledger.errors)

    if tag is not None:
        try:
            gate_required = requires_day0_gate(tag)
        except ValueError as exc:
            return GateResult(False, False, None, (), None, (), (f"invalid --tag: {exc}",))
        if not gate_required:
            return GateResult(True, True, f"day-0 gate applies only to stable >= 2.0.0 releases; skipping for {tag}", (), None, (), ())

    if commit is not None and not _is_hex40(commit):
        return GateResult(False, False, None, (), None, (), (f"invalid --commit {commit!r}: expected 40-hex SHA",))

    errors: list[str] = []
    repos = check_all_repositories(fetcher=fetcher, timeout=timeout, max_bytes=max_bytes)
    errors.extend(r.error for r in repos if not r.passed and r.error)

    ledger = validate_ledger_file(ledger_path)
    errors.extend(ledger.errors)

    evidence_errors: tuple[str, ...] = ()
    if evidence_path is not None:
        effective_pin = source_pin if source_pin is not None else read_cargo_source_pin(cargo_manifest_path)
        evidence_errors = validate_evidence_file(
            evidence_path, expected_tag=tag, expected_commit=commit,
            expected_source_pin=effective_pin, live_ledger=ledger, live_repos=repos,
            expected_release_assets=expected_release_assets,
        )
        errors.extend(evidence_errors)
    elif require_evidence:
        msg = "candidate-bound release evidence manifest (--evidence <path>) is required"
        evidence_errors = (msg,)
        errors.append(msg)

    return GateResult(not errors, False, None, repos, ledger, evidence_errors, tuple(errors))


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--tag", help="release tag (e.g. v2.0.0 or v2.0.0-alpha.2)")
    p.add_argument("--commit", help="expected 40-hex candidate commit SHA")
    p.add_argument("--evidence", type=Path, help="path to candidate-bound release evidence manifest JSON")
    p.add_argument("--require-evidence", action="store_true", help="fail closed if --evidence is omitted when gate applies")
    p.add_argument("--ledger", type=Path, default=DEFAULT_LEDGER, help="path to DIVERGENCES.md")
    p.add_argument("--manifest", type=Path, default=DEFAULT_CARGO_MANIFEST, help="path to root Cargo.toml")
    p.add_argument("--source-pin", help="expected 40-hex M4b upstream source pin override")
    p.add_argument("--ledger-only", action="store_true", help="diagnostic mode: validate DIVERGENCES.md only")
    p.add_argument("--timeout", type=float, default=DEFAULT_TIMEOUT_SECONDS, help="HTTP timeout in seconds")
    p.add_argument("--json", action="store_true", help="print machine-readable JSON summary to stdout")
    return p


def main(argv: list[str] | None = None, *, fetcher: Fetcher = default_fetcher) -> int:
    args = build_parser().parse_args(argv)
    result = run_day0_gate(
        tag=args.tag, commit=args.commit, evidence_path=args.evidence, require_evidence=args.require_evidence,
        ledger_path=args.ledger, cargo_manifest_path=args.manifest, source_pin=args.source_pin,
        ledger_only=args.ledger_only, fetcher=fetcher, timeout=args.timeout,
    )
    if args.json:
        ledger_summary = None if result.ledger is None else {
            "passed": result.ledger.passed, "sha256": result.ledger.sha256,
            "counts": result.ledger.counts, "row_count": len(result.ledger.rows),
            "errors": list(result.ledger.errors),
        }
        print(json.dumps({
            "passed": result.passed, "skipped": result.skipped, "reason": result.reason,
            "repositories": [r._asdict() for r in result.repos], "ledger": ledger_summary,
            "evidence_errors": list(result.evidence_errors), "errors": list(result.errors),
        }, indent=2))
    elif result.skipped:
        print(result.reason)
    elif result.passed:
        parts = []
        if result.repos:
            parts.append(f"{len(result.repos)}/{len(result.repos)} day-0 repositories verified")
        if result.ledger is not None:
            c = result.ledger.counts
            parts.append(f"ledger ok ({len(result.ledger.rows)} permanent rows: security={c['security']}, contract={c['contract']}, licensing={c['licensing']})")
        if args.evidence is not None:
            parts.append(f"evidence manifest ok ({args.evidence})")
        print("day-0 gate PASS: " + "; ".join(parts))
    else:
        for err in result.errors:
            print(f"day-0 gate FAIL: {err}", file=sys.stderr)
    return 0 if result.passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
