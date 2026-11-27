"""Candidate-bound format-1 release evidence manifest validator (A8.4 + A8.6).

Imported by scripts/check-day0.py; keeps evidence schema validation modular
and well within the repository's 1,000-line maintained-file limit.
"""

from datetime import datetime
import json
import math
from pathlib import Path
import re
from typing import Any, Callable


MAX_EVIDENCE_BYTES = 16 * 1024 * 1024  # 16 MiB

CATALOG_REPOS = ("18-lab-dev/vosu-tools", "18-lab-dev/vosulay", "18-lab-dev/vosu-overlays")
QUICKSTART_REPO = "18-lab-dev/vosu-overlay-quickstart"
ALL_DAY0_REPOS = (*CATALOG_REPOS, QUICKSTART_REPO)
ALLOWED_RECEIPT_REPOS = frozenset({*ALL_DAY0_REPOS, "18-lab-dev/vosu"})

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
RECEIPT_KINDS = frozenset({
    "own-ci-process", "native-sandbox", "browser",
    "package", "sdk-publication", "release", "live",
})
RECEIPT_RUNTIMES = frozenset({"process", "in-process", "none"})
HOST_SOURCES = frozenset({"github-release", "local-build"})
PACKAGE_KINDS = frozenset({"plugin", "tool", "overlay"})
CATALOG_FILES = frozenset({"catalog-v2.json", "catalog.json"})

SUPERVISOR_PLUGIN_ID = "vosu.vosulay"
SUPERVISOR_ALLOWED_REPOS = frozenset({"18-lab-dev/vosulay", "18-lab-dev/vosu"})
TOOLS_OWNER_REPO = "18-lab-dev/vosu-tools"
REQUIRED_TOOL_PLUGINS = (
    "vosu.tools.lazer-library",
    "vosu.tools.map-analysis",
    "vosu.tools.server-pp",
    "vosu.tools.practice",
    "vosu.tools.replay-renderer",
    "vosu.tools.skin-studio",
    "vosu.tools.downloader",
)


def is_nonempty_str(value: Any) -> bool:
    return isinstance(value, str) and bool(value.strip())


def is_hex40(value: Any) -> bool:
    return isinstance(value, str) and bool(HEX40_RE.match(value))


def is_hex64(value: Any) -> bool:
    return isinstance(value, str) and bool(HEX64_RE.match(value))


def is_rfc3339(value: Any) -> bool:
    if not isinstance(value, str) or not RFC3339_RE.match(value):
        return False
    try:
        datetime.fromisoformat(value[:-1] + "+00:00" if value.endswith(("Z", "z")) else value)
        return True
    except ValueError:
        return False


def is_int(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def is_finite_nonneg(value: Any) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value) and value >= 0


def _in_str_set(value: Any, allowed: set[str] | frozenset[str] | tuple[str, ...] | dict[str, Any]) -> bool:
    return isinstance(value, str) and value in allowed


def _exact_str_set(value: Any, expected: frozenset[str]) -> bool:
    return (
        isinstance(value, list)
        and len(value) == len(expected)
        and all(isinstance(item, str) for item in value)
        and set(value) == expected
    )


def _chk(cond: bool, msg: str, errors: list[str]) -> bool:
    if not cond:
        errors.append(msg)
    return cond


def _validate_ecosystem_receipts(
    receipts: Any,
    *,
    day0_repo_commits: dict[str, str],
    candidate_commit: str | None,
    parse_semver_tag: Callable[..., tuple[tuple[int, int, int], str | None]],
    errors: list[str],
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
        receipt_ok = _chk(is_int(r.get("receipt_version")) and r.get("receipt_version") == 1, f"{ctx}.receipt_version must be 1", errors)
        kind, repo, commit = r.get("kind"), r.get("repository"), r.get("commit")
        receipt_ok &= _chk(_in_str_set(kind, RECEIPT_KINDS), f"{ctx}.kind invalid: {kind!r}", errors)
        repo_ok = _chk(
            _in_str_set(repo, ALLOWED_RECEIPT_REPOS),
            f"{ctx}.repository must be one of {sorted(ALLOWED_RECEIPT_REPOS)}, got {repo!r}",
            errors,
        )
        receipt_ok &= repo_ok
        if _chk(is_hex40(commit), f"{ctx}.commit must be 40-hex", errors):
            if repo_ok and repo in day0_repo_commits:
                receipt_ok &= _chk(commit == day0_repo_commits[repo], f"{ctx}.commit does not match day0_check commit for {repo}", errors)
            elif repo == "18-lab-dev/vosu" and candidate_commit:
                receipt_ok &= _chk(commit == candidate_commit, f"{ctx}.commit does not match candidate.commit", errors)
        else:
            receipt_ok = False
        receipt_ok &= _chk(r.get("tag") is None or is_nonempty_str(r.get("tag")), f"{ctx}.tag must be null or string", errors)

        host = r.get("host")
        if _chk(isinstance(host, dict), f"{ctx}.host must be an object", errors):
            host_tag = host.get("tag")
            try:
                (host_major, _mi, _pa), _pre = parse_semver_tag(host_tag, require_v_prefix=True)
            except ValueError:
                errors.append(f"{ctx}.host.tag must be a valid SemVer v-tag, got {host_tag!r}")
                receipt_ok = False
            else:
                if _chk(host_major >= 2, f"{ctx}.host.tag {host_tag!r} must have major >= 2", errors):
                    if kind == "own-ci-process":
                        process_host_tags.add(host_tag)
                else:
                    receipt_ok = False
            receipt_ok &= _chk(is_hex64(host.get("binary_sha256")), f"{ctx}.host.binary_sha256 must be 64-hex", errors)
            receipt_ok &= _chk(_in_str_set(host.get("source"), HOST_SOURCES), f"{ctx}.host.source invalid", errors)
        else:
            receipt_ok = False

        sdk = r.get("sdk")
        if sdk is not None:
            if _chk(isinstance(sdk, dict), f"{ctx}.sdk must be an object or null", errors):
                receipt_ok &= _chk(is_nonempty_str(sdk.get("version")), f"{ctx}.sdk.version required", errors)
                receipt_ok &= _chk(is_hex64(sdk.get("crate_sha256")), f"{ctx}.sdk.crate_sha256 must be 64-hex", errors)
            else:
                receipt_ok = False

        target, runtime, child = r.get("target"), r.get("runtime"), r.get("child")
        target_ok = _chk(_in_str_set(target, ALL_TARGETS), f"{ctx}.target invalid: {target!r}", errors)
        receipt_ok &= target_ok
        receipt_ok &= _chk(_in_str_set(runtime, RECEIPT_RUNTIMES), f"{ctx}.runtime invalid: {runtime!r}", errors)
        is_fixture = isinstance(child, str) and child.startswith("fixture:") and bool(child[8:].strip())
        receipt_ok &= _chk(child is None or child == "production" or is_fixture, f"{ctx}.child invalid: {child!r}", errors)

        package_ids: set[str] = set()
        if _chk(isinstance(r.get("packages"), list), f"{ctx}.packages must be a list", errors):
            for p_idx, pkg in enumerate(r["packages"]):
                p_ctx = f"{ctx}.packages[{p_idx}]"
                if not _chk(isinstance(pkg, dict), f"{p_ctx} must be an object", errors):
                    receipt_ok = False
                    continue
                if _chk(is_nonempty_str(pkg.get("id")), f"{p_ctx}.id required", errors):
                    package_ids.add(pkg["id"])
                else:
                    receipt_ok = False
                receipt_ok &= _chk(_in_str_set(pkg.get("kind"), PACKAGE_KINDS), f"{p_ctx}.kind invalid", errors)
                receipt_ok &= _chk(is_nonempty_str(pkg.get("version")) and is_nonempty_str(pkg.get("file")), f"{p_ctx} version/file required", errors)
                pkg_target = pkg.get("target")
                if _chk(_in_str_set(pkg_target, ALL_TARGETS), f"{p_ctx}.target invalid", errors):
                    if target_ok and target != "any":
                        receipt_ok &= _chk(
                            pkg_target in (target, "any"),
                            f"{p_ctx}.target {pkg_target!r} mismatches receipt target {target!r}",
                            errors,
                        )
                else:
                    receipt_ok = False
                receipt_ok &= _chk(is_hex64(pkg.get("sha256")) and is_int(pkg.get("size")) and pkg["size"] > 0, f"{p_ctx} sha256/size invalid", errors)
        else:
            receipt_ok = False

        if _chk(isinstance(r.get("catalogs"), list), f"{ctx}.catalogs must be a list", errors):
            for c_idx, cat in enumerate(r["catalogs"]):
                c_ctx = f"{ctx}.catalogs[{c_idx}]"
                if _chk(isinstance(cat, dict), f"{c_ctx} must be an object", errors):
                    receipt_ok &= _chk(
                        _in_str_set(cat.get("file"), CATALOG_FILES) and is_hex64(cat.get("sha256")) and is_int(cat.get("size")) and cat["size"] > 0,
                        f"{c_ctx} invalid",
                        errors,
                    )
                else:
                    receipt_ok = False
        else:
            receipt_ok = False

        wf = r.get("workflow")
        if _chk(isinstance(wf, dict), f"{ctx}.workflow must be an object", errors):
            receipt_ok &= _chk(is_nonempty_str(wf.get("run_url")) and wf["run_url"].startswith("https://"), f"{ctx}.workflow.run_url must be https://", errors)
            receipt_ok &= _chk(
                is_int(wf.get("run_id")) and wf["run_id"] > 0 and is_int(wf.get("attempt")) and wf["attempt"] > 0 and is_nonempty_str(wf.get("job")),
                f"{ctx}.workflow fields invalid",
                errors,
            )
        else:
            receipt_ok = False

        tests_ok = _chk(
            isinstance(r.get("tests"), list) and bool(r.get("tests")),
            f"{ctx}.tests must be non-empty (cannot infer pass from an asset name)",
            errors,
        )
        if tests_ok:
            for t_idx, t in enumerate(r["tests"]):
                t_ctx = f"{ctx}.tests[{t_idx}]"
                if not _chk(isinstance(t, dict), f"{t_ctx} must be an object", errors):
                    tests_ok = False
                    continue
                sel, pas, fail, skip = t.get("selected"), t.get("passed"), t.get("failed"), t.get("skipped")
                t_valid = (
                    _chk(is_nonempty_str(t.get("suite")) and is_nonempty_str(t.get("command")), f"{t_ctx} suite/command required", errors)
                    and _chk(all(is_int(v) and v >= 0 for v in (sel, pas, fail, skip)), f"{t_ctx} counts must be >= 0 ints", errors)
                )
                if t_valid:
                    t_valid = (
                        _chk(sel > 0 and pas > 0 and fail == 0, f"{t_ctx} must have selected>0, passed>0, failed==0", errors)
                        and _chk(pas + fail + skip == sel, f"{t_ctx}: passed+failed+skipped != selected", errors)
                    )
                tests_ok &= t_valid
        receipt_ok &= tests_ok

        nat = r.get("native")
        receipt_ok &= _chk(isinstance(nat, dict) and isinstance(nat.get("sandbox"), dict) and isinstance(nat.get("cleanup"), list), f"{ctx}.native invalid", errors)
        receipt_ok &= _chk(isinstance(r.get("approvals"), list) and all(is_nonempty_str(a) for a in r["approvals"]), f"{ctx}.approvals invalid", errors)
        receipt_ok &= _chk(r.get("result") == "pass", f"{ctx}.result must be 'pass', got {r.get('result')!r}", errors)
        receipt_ok &= _chk(is_rfc3339(r.get("started_at")) and is_rfc3339(r.get("finished_at")), f"{ctx} timestamps must be RFC 3339", errors)

        # Bind official plugins to their owning repository.
        tools = package_ids & set(REQUIRED_TOOL_PLUGINS)
        if tools:
            receipt_ok &= _chk(
                repo == TOOLS_OWNER_REPO,
                f"{ctx}: official tool plugins {sorted(tools)} must come from {TOOLS_OWNER_REPO}, got {repo!r}",
                errors,
            )
        if SUPERVISOR_PLUGIN_ID in package_ids:
            receipt_ok &= _chk(
                _in_str_set(repo, SUPERVISOR_ALLOWED_REPOS),
                f"{ctx}: {SUPERVISOR_PLUGIN_ID} must come from {sorted(SUPERVISOR_ALLOWED_REPOS)}, got {repo!r}",
                errors,
            )

        passed_native = receipt_ok and _in_str_set(target, NATIVE_TARGETS)
        if kind == "own-ci-process":
            _chk(runtime == "process", f"{ctx}: own-ci-process must have runtime='process', got {runtime!r}", errors)
            _chk(_in_str_set(target, NATIVE_TARGETS) and bool(package_ids), f"{ctx}: own-ci-process requires native target and packages", errors)
            if SUPERVISOR_PLUGIN_ID in package_ids:
                if _chk(is_fixture, f"{ctx}: A8.6 requires {SUPERVISOR_PLUGIN_ID} own-ci-process child='fixture:<id>'", errors) and passed_native and runtime == "process":
                    supervisor_process_targets.add(target)
            if tools and _chk(child == "production", f"{ctx}: official plugin own-ci-process requires child='production'", errors) and passed_native and runtime == "process":
                tool_process_coverage.update((pid, target) for pid in tools)

        if kind == "native-sandbox":
            _chk(_in_str_set(target, NATIVE_TARGETS), f"{ctx}: native-sandbox must target native OS, got {target!r}", errors)
            if SUPERVISOR_PLUGIN_ID in package_ids:
                ok_rt = _chk(runtime == "in-process", f"{ctx}: A8.6 requires {SUPERVISOR_PLUGIN_ID} native-sandbox runtime='in-process'", errors)
                ok_ch = _chk(child == "production", f"{ctx}: A8.6 forbids fixture child as production CEF evidence", errors)
                if passed_native and ok_rt and ok_ch:
                    supervisor_sandbox_targets.add(target)

    _chk(len(process_host_tags) <= 1, f"ecosystem_receipts mix inconsistent own-ci-process host tags: {sorted(process_host_tags)}", errors)
    for target in NATIVE_TARGETS:
        _chk(target in supervisor_process_targets, f"A8.6 incomplete: missing {SUPERVISOR_PLUGIN_ID} own-ci-process (fixture) for {target}", errors)
        _chk(target in supervisor_sandbox_targets, f"A8.6 incomplete: missing {SUPERVISOR_PLUGIN_ID} native-sandbox (in-process production) for {target}", errors)
        for pid in REQUIRED_TOOL_PLUGINS:
            _chk((pid, target) in tool_process_coverage, f"missing own-ci-process receipt for {pid} on {target}", errors)


def _validate_native_verification(
    nv: Any,
    *,
    cand_commit: str | None,
    artifact_hashes_by_target: dict[str, set[str]],
    errors: list[str],
) -> None:
    if not _chk(isinstance(nv, dict), "native_verification must be an object", errors):
        return
    for field in ("linux_prepush_sha", "windows_native_sha"):
        sha = nv.get(field)
        if _chk(is_hex40(sha), f"native_verification.{field} must be 40-hex", errors) and cand_commit:
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
            if _chk(_in_str_set(plat, allowed) and plat not in seen, f"{ctx}.{plat_key} invalid/duplicate: {plat!r}", errors):
                seen.add(plat)
            item_fn(ctx, item)
            _chk(item.get("result") == "pass", f"{ctx}.result must be 'pass'", errors)
        missing = sorted(set(allowed) - seen)
        _chk(not missing, f"native_verification.{key} missing: {', '.join(missing)}", errors)

    def _smoke_item(ctx: str, item: dict) -> None:
        plat, b_sha = item.get("platform"), item.get("binary_sha256")
        if _chk(is_hex64(b_sha), f"{ctx}.binary_sha256 must be 64-hex", errors) and _in_str_set(plat, PLATFORM_TO_TARGET):
            _chk(b_sha in artifact_hashes_by_target[PLATFORM_TO_TARGET[plat]], f"{ctx}.binary_sha256 does not match any candidate artifact", errors)
        _chk(is_hex64(item.get("child_fixture_sha256")), f"{ctx}.child_fixture_sha256 must be 64-hex", errors)
        for flag in ("api_ingame_404", "consoleless_lifecycle_stop", "no_orphans"):
            _chk(item.get(flag) is True, f"{ctx}.{flag} must be true", errors)

    def _updater_item(ctx: str, item: dict) -> None:
        plat, c_sha = item.get("platform"), item.get("candidate_sha256")
        _chk(is_hex64(item.get("source_binary_sha256")), f"{ctx}.source_binary_sha256 must be 64-hex", errors)
        if _chk(is_hex64(c_sha), f"{ctx}.candidate_sha256 must be 64-hex", errors) and _in_str_set(plat, PLATFORM_TO_TARGET):
            _chk(c_sha in artifact_hashes_by_target[PLATFORM_TO_TARGET[plat]], f"{ctx}.candidate_sha256 does not match any candidate artifact", errors)
        for flag in ("decline_no_download", "accept_upgrade", "rollback"):
            _chk(item.get(flag) is True, f"{ctx}.{flag} must be true", errors)

    def _session_item(ctx: str, item: dict) -> None:
        _chk(is_nonempty_str(item.get("client_version")), f"{ctx}.client_version required", errors)
        _chk(_exact_str_set(item.get("states_traversed"), REQUIRED_PARITY_STATES), f"{ctx}.states_traversed incomplete", errors)
        ids = item.get("installed_ids")
        if _chk(isinstance(ids, list) and bool(ids) and all(is_nonempty_str(x) for x in ids), f"{ctx}.installed_ids required", errors):
            _chk("vosulay-editor" in ids and SUPERVISOR_PLUGIN_ID in ids, f"{ctx}.installed_ids missing vosulay-editor/{SUPERVISOR_PLUGIN_ID}", errors)
        _chk(item.get("cef_sandbox_verified") is True and item.get("cleanup_verified") is True, f"{ctx} cef_sandbox_verified/cleanup_verified must be true", errors)

    _check_platform_list("packaged_smoke", set(PLATFORM_TO_TARGET), _smoke_item)
    _check_platform_list("updater_1_2_4", set(PLATFORM_TO_TARGET), _updater_item)
    _check_platform_list("owner_live_sessions", REQUIRED_M4B_PAIRS, _session_item)
    _check_platform_list("d15_forge_receipts", REQUIRED_FORGES, lambda ctx, item: _chk(is_rfc3339(item.get("checked_at")), f"{ctx}.checked_at must be RFC 3339", errors))

    pkgs = nv.get("packages")
    if _chk(isinstance(pkgs, list) and bool(pkgs), "native_verification.packages must be non-empty", errors):
        for idx, item in enumerate(pkgs):
            ctx = f"native_verification.packages[{idx}]"
            if not _chk(isinstance(item, dict), f"{ctx} must be an object", errors):
                continue
            _chk(
                _in_str_set(item.get("manager"), PACKAGE_MANAGERS) and is_nonempty_str(item.get("owner_repo")) and is_nonempty_str(item.get("package_id")),
                f"{ctx} manager/owner_repo/package_id invalid",
                errors,
            )
            _chk(
                is_hex64(item.get("artifact_sha256")) and item.get("install_upgrade_uninstall") is True and item.get("result") == "pass",
                f"{ctx} artifact_sha256/install_upgrade_uninstall/result invalid",
                errors,
            )


def validate_evidence_manifest(
    manifest: Any,
    *,
    parse_semver_tag: Callable[..., tuple[tuple[int, int, int], str | None]],
    requires_day0_gate: Callable[[str], bool],
    parse_version: Callable[[str], tuple[str, str | None]],
    expected_tag: str | None = None,
    expected_commit: str | None = None,
    expected_source_pin: str | None = None,
    live_ledger: Any = None,
    live_repos: tuple[Any, ...] | None = None,
    expected_release_assets: dict[str, str] | None = None,
) -> tuple[str, ...]:
    """Validate a candidate-bound format-1 release evidence manifest (A8.4 + A8.6)."""
    if not isinstance(manifest, dict):
        return ("evidence manifest root must be a JSON object",)
    errors: list[str] = []
    _chk(is_int(manifest.get("manifest_version")) and manifest.get("manifest_version") == 1, "manifest_version must be integer 1", errors)

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
        if _chk(is_hex40(cand.get("commit")), "candidate.commit must be 40-hex", errors):
            cand_commit = cand["commit"]
            if expected_commit is not None:
                _chk(cand_commit == expected_commit, f"candidate.commit {cand_commit!r} != expected {expected_commit!r}", errors)
        _chk(is_rfc3339(cand.get("created_at")), "candidate.created_at must be RFC 3339", errors)
        sdk = cand.get("sdk")
        if _chk(isinstance(sdk, dict), "candidate.sdk must be an object", errors):
            sdk_ver = sdk.get("version")
            try:
                if not isinstance(sdk_ver, str):
                    raise ValueError("not a string")
                parse_version(sdk_ver)
            except (TypeError, ValueError):
                errors.append(f"candidate.sdk.version must be valid SemVer, got {sdk_ver!r}")
            _chk(is_hex64(sdk.get("crate_sha256")), "candidate.sdk.crate_sha256 must be 64-hex", errors)
            _chk(is_hex64(sdk.get("header_sha256")), "candidate.sdk.header_sha256 must be 64-hex", errors)

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
            ok_name = _chk(is_nonempty_str(name), f"{ctx}.file required", errors)
            if ok_name:
                _chk(name not in artifact_by_file, f"{ctx}.file duplicate {name!r}", errors)
            ok_target = _chk(_in_str_set(target, ALL_TARGETS), f"{ctx}.target invalid", errors)
            ok_sha = _chk(is_hex64(sha), f"{ctx}.sha256 must be 64-hex", errors)
            _chk(is_int(art.get("size")) and art["size"] > 0, f"{ctx}.size must be > 0", errors)
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
        _chk(is_rfc3339(day0.get("checked_at")), "day0_check.checked_at must be RFC 3339", errors)
        l_sha = day0.get("ledger_sha256")
        if _chk(is_hex64(l_sha), "day0_check.ledger_sha256 must be 64-hex", errors) and live_ledger and live_ledger.passed:
            _chk(l_sha == live_ledger.sha256, "day0_check.ledger_sha256 does not match current DIVERGENCES.md", errors)
        lc = day0.get("ledger_counts")
        if _chk(isinstance(lc, dict), "day0_check.ledger_counts must be an object", errors):
            valid_counts = all(
                _chk(is_int(lc.get(k)) and lc[k] >= 0, f"day0_check.ledger_counts.{k} must be >= 0 int", errors)
                for k in ("security", "contract", "licensing", "temporary")
            )
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
                if not _chk(_in_str_set(rn, ALL_DAY0_REPOS) and rn not in seen_repos, f"{ctx}.repository invalid or duplicate: {rn!r}", errors):
                    continue
                seen_repos.add(rn)
                rt, rc, rid = entry.get("tag"), entry.get("commit"), entry.get("release_id")
                _chk(is_nonempty_str(rt), f"{ctx}.tag required", errors)
                if _chk(is_hex40(rc), f"{ctx}.commit must be 40-hex", errors):
                    day0_repo_commits[rn] = rc
                _chk(is_int(rid) and rid > 0, f"{ctx}.release_id must be > 0", errors)
                _chk(is_rfc3339(entry.get("checked_at")), f"{ctx}.checked_at must be RFC 3339", errors)
                if rn == QUICKSTART_REPO:
                    try:
                        (maj, _mi, _pa), pre = parse_semver_tag(rt, require_v_prefix=False)
                        _chk(pre is None and maj == 2, f"{ctx}.tag {rt!r} must be stable SemVer major 2", errors)
                    except ValueError as exc:
                        errors.append(f"{ctx}.tag invalid SemVer: {exc}")
                    _chk(entry.get("catalog_v2_sha256") is None, f"{ctx}.catalog_v2_sha256 must be null for quickstart", errors)
                    _chk(entry.get("catalog_v1_sha256") is None, f"{ctx}.catalog_v1_sha256 must be null for quickstart", errors)
                else:
                    _chk(is_hex64(entry.get("catalog_v2_sha256")), f"{ctx}.catalog_v2_sha256 must be 64-hex", errors)
                    _chk(is_hex64(entry.get("catalog_v1_sha256")), f"{ctx}.catalog_v1_sha256 must be 64-hex", errors)
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
        parse_semver_tag=parse_semver_tag,
        errors=errors,
    )

    # 5. m4b_parity
    m4b = manifest.get("m4b_parity")
    if _chk(isinstance(m4b, dict), "m4b_parity must be an object", errors):
        pin = m4b.get("source_pin")
        if _chk(is_hex40(pin), "m4b_parity.source_pin must be 40-hex", errors) and expected_source_pin is not None:
            _chk(pin == expected_source_pin, f"m4b_parity.source_pin {pin!r} != Cargo pin {expected_source_pin!r}", errors)
        _chk(is_hex64(m4b.get("inventory_sha256")), "m4b_parity.inventory_sha256 must be 64-hex", errors)
        pairs = m4b.get("pairs")
        if _chk(isinstance(pairs, list), "m4b_parity.pairs must be a list", errors):
            seen_pairs: set[str] = set()
            for idx, p in enumerate(pairs):
                ctx = f"m4b_parity.pairs[{idx}]"
                if not _chk(isinstance(p, dict), f"{ctx} must be an object", errors):
                    continue
                plat = p.get("platform")
                if _chk(_in_str_set(plat, REQUIRED_M4B_PAIRS) and plat not in seen_pairs, f"{ctx}.platform invalid/duplicate: {plat!r}", errors):
                    seen_pairs.add(plat)
                _chk(is_nonempty_str(p.get("authorization_ref")), f"{ctx}.authorization_ref required", errors)
                if _chk(is_hex40(p.get("vosu_commit")), f"{ctx}.vosu_commit must be 40-hex", errors) and cand_commit:
                    _chk(p["vosu_commit"] == cand_commit, f"{ctx}.vosu_commit != candidate.commit", errors)
                _chk(is_nonempty_str(p.get("client_version")), f"{ctx}.client_version required", errors)
                _chk(_exact_str_set(p.get("states"), REQUIRED_PARITY_STATES), f"{ctx}.states must cover all 4 states", errors)
                _chk(is_int(p.get("case_count")) and p["case_count"] > 0, f"{ctx}.case_count must be > 0", errors)
                _chk(is_hex64(p.get("capture_sha256")), f"{ctx}.capture_sha256 must be 64-hex", errors)
                _chk(p.get("result") == "pass", f"{ctx}.result must be 'pass'", errors)
            missing_pairs = sorted(REQUIRED_M4B_PAIRS - seen_pairs)
            _chk(not missing_pairs, f"m4b_parity.pairs missing: {', '.join(missing_pairs)}", errors)

    # 6. d17_performance
    d17 = manifest.get("d17_performance")
    if _chk(isinstance(d17, dict), "d17_performance must be an object", errors):
        for field in ("decision", "cumulative_verdict", "build_budget_verdict"):
            _chk(d17.get(field) == "pass", f"d17_performance.{field} must be 'pass'", errors)
        for field in ("baseline_pre_m0_commit", "baseline_prev_commit", "candidate_commit"):
            _chk(is_hex40(d17.get(field)), f"d17_performance.{field} must be 40-hex", errors)
        if cand_commit and is_hex40(d17.get("candidate_commit")):
            _chk(d17["candidate_commit"] == cand_commit, "d17_performance.candidate_commit != candidate.commit", errors)
        for field in ("linux_receipt_dir", "windows_receipt_dir"):
            _chk(is_nonempty_str(d17.get(field)), f"d17_performance.{field} required", errors)
        for field in ("broker_linux_ratios", "broker_windows_ratios"):
            ratios = d17.get(field)
            if _chk(isinstance(ratios, dict), f"d17_performance.{field} must be an object", errors):
                pr, rg, cap = ratios.get("precise"), ratios.get("regular"), ratios.get("cap")
                if _chk(all(is_finite_nonneg(v) for v in (pr, rg, cap)) and cap > 0, f"d17_performance.{field} invalid numbers", errors):
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
    parse_semver_tag: Callable[..., tuple[tuple[int, int, int], str | None]],
    requires_day0_gate: Callable[[str], bool],
    parse_version: Callable[[str], tuple[str, str | None]],
    expected_tag: str | None = None,
    expected_commit: str | None = None,
    expected_source_pin: str | None = None,
    live_ledger: Any = None,
    live_repos: tuple[Any, ...] | None = None,
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
        data,
        parse_semver_tag=parse_semver_tag,
        requires_day0_gate=requires_day0_gate,
        parse_version=parse_version,
        expected_tag=expected_tag,
        expected_commit=expected_commit,
        expected_source_pin=expected_source_pin,
        live_ledger=live_ledger,
        live_repos=live_repos,
        expected_release_assets=expected_release_assets,
    )
