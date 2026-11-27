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
import hashlib
import importlib.util
import json
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
MAX_REDIRECTS = 3

REQUIRED_CATALOG_ASSET = "catalog-v2.json"

LEDGER_HEADER = ("id", "class", "scope", "reason", "closed by")
PERMANENT_LEDGER_CLASSES = frozenset({"security", "contract", "licensing"})
DEFINED_LEDGER_CLASSES = PERMANENT_LEDGER_CLASSES | frozenset({"temporary"})
SEPARATOR_CELL_RE = re.compile(r"^:?-{3,}:?$")


def _load_sibling(module_name: str, filename: str):
    path = Path(__file__).with_name(filename)
    spec = importlib.util.spec_from_file_location(module_name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


_PACKAGE_RELEASE = _load_sibling("package_release", "package-release.py")
_EVIDENCE = _load_sibling("day0_evidence", "day0_evidence.py")

# Re-export shared constants & helpers from day0_evidence for callers/tests.
MAX_EVIDENCE_BYTES = _EVIDENCE.MAX_EVIDENCE_BYTES
CATALOG_REPOS = _EVIDENCE.CATALOG_REPOS
QUICKSTART_REPO = _EVIDENCE.QUICKSTART_REPO
ALL_DAY0_REPOS = _EVIDENCE.ALL_DAY0_REPOS
ALLOWED_RECEIPT_REPOS = _EVIDENCE.ALLOWED_RECEIPT_REPOS
NATIVE_TARGETS = _EVIDENCE.NATIVE_TARGETS
ALL_TARGETS = _EVIDENCE.ALL_TARGETS
SUPERVISOR_PLUGIN_ID = _EVIDENCE.SUPERVISOR_PLUGIN_ID
REQUIRED_TOOL_PLUGINS = _EVIDENCE.REQUIRED_TOOL_PLUGINS
HEX40_RE = _EVIDENCE.HEX40_RE
HEX64_RE = _EVIDENCE.HEX64_RE
_is_nonempty_str = _EVIDENCE.is_nonempty_str
_is_hex40 = _EVIDENCE.is_hex40
_is_hex64 = _EVIDENCE.is_hex64
_is_int = _EVIDENCE.is_int


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
    return _EVIDENCE.validate_evidence_manifest(
        manifest,
        parse_semver_tag=parse_semver_tag,
        requires_day0_gate=requires_day0_gate,
        parse_version=_PACKAGE_RELEASE.parse_version,
        expected_tag=expected_tag,
        expected_commit=expected_commit,
        expected_source_pin=expected_source_pin,
        live_ledger=live_ledger,
        live_repos=live_repos,
        expected_release_assets=expected_release_assets,
    )


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
    return _EVIDENCE.validate_evidence_file(
        path,
        parse_semver_tag=parse_semver_tag,
        requires_day0_gate=requires_day0_gate,
        parse_version=_PACKAGE_RELEASE.parse_version,
        expected_tag=expected_tag,
        expected_commit=expected_commit,
        expected_source_pin=expected_source_pin,
        live_ledger=live_ledger,
        live_repos=live_repos,
        expected_release_assets=expected_release_assets,
        max_bytes=max_bytes,
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
            return GateResult(
                False, False, None, (), None, (),
                ("--ledger-only cannot be combined with --tag, --evidence, or --require-evidence",),
            )
        ledger = validate_ledger_file(ledger_path)
        return GateResult(ledger.passed, False, None, (), ledger, (), ledger.errors)

    if tag is not None:
        try:
            gate_required = requires_day0_gate(tag)
        except ValueError as exc:
            return GateResult(False, False, None, (), None, (), (f"invalid --tag: {exc}",))
        if not gate_required:
            return GateResult(
                True, True,
                f"day-0 gate applies only to stable >= 2.0.0 releases; skipping for {tag}",
                (), None, (), (),
            )

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
            parts.append(
                f"ledger ok ({len(result.ledger.rows)} permanent rows: "
                f"security={c['security']}, contract={c['contract']}, licensing={c['licensing']})"
            )
        if args.evidence is not None:
            parts.append(f"evidence manifest ok ({args.evidence})")
        print("day-0 gate PASS: " + "; ".join(parts))
    else:
        for err in result.errors:
            print(f"day-0 gate FAIL: {err}", file=sys.stderr)
    return 0 if result.passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
