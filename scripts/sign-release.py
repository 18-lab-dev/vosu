#!/usr/bin/env python3
"""Sign a GitHub release's assets with the OpenPGP release key.

Run locally after the release workflow has uploaded the assets. Each asset is
downloaded and checked against the SHA-256 digest GitHub recorded at upload.
Unsigned assets get a detached binary signature, uploaded as `<asset>.sig`.
Assets that already have one are re-verified against their current bytes and
the release key, so a replaced asset with a stale signature fails instead of
passing. Existing assets and signatures are never overwritten.

Draft releases are supported; `--publish` publishes the draft once every asset
is signed. A repository with immutable releases must sign the draft this way,
because a published immutable release rejects new assets.

The private key stays on the maintainer's machine. Signing in CI would put it
in a repository secret, where anyone able to change a workflow could use it.
"""

import argparse
import hashlib
import importlib.util
import json
import subprocess
import tempfile
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("package_release", Path(__file__).with_name("package-release.py"))
release_version = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release_version)

DAY0_SPEC = importlib.util.spec_from_file_location("check_day0", Path(__file__).with_name("check-day0.py"))
day0_gate = importlib.util.module_from_spec(DAY0_SPEC)
DAY0_SPEC.loader.exec_module(day0_gate)

RELEASE_KEY = "6F0F0D30C09D3A036246A8102CB6524BB54B2CDA"


def run(*command, capture=False):
    result = subprocess.run(command, check=True, text=True, capture_output=capture)
    return result.stdout if capture else None


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as stream:
        for block in iter(lambda: stream.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def signer(path, signature):
    """Return the primary-key fingerprint of a valid signature, else None."""
    result = subprocess.run(
        ["gpg", "--status-fd", "1", "--verify", str(signature), str(path)],
        capture_output=True, text=True,
    )
    if result.returncode != 0:
        return None
    for line in result.stdout.splitlines():
        fields = line.split()
        # VALIDSIG <subkey-fpr> ... <primary-key-fpr> is the last field.
        if fields[:2] == ["[GNUPG:]", "VALIDSIG"]:
            return fields[-1]
    return None


def require_signed_by(path, signature, key):
    if signer(path, signature) != key:
        raise SystemExit(f"{path.name}: {signature.name} is not a valid signature by {key}")


def sign(path, key):
    signature = path.with_name(path.name + ".sig")
    run("gpg", "--local-user", key, "--detach-sign", "--output", str(signature), str(path))
    require_signed_by(path, signature, key)
    return signature


def is_prerelease_tag(tag):
    """True for a prerelease tag (e.g. v2.0.0-alpha.1), false for a stable one.

    Uses the packager's SemVer grammar, so a tag the release workflow rejects
    fails here instead of being published with guessed flags.
    """
    if not tag.startswith("v"):
        raise ValueError(f"release tag must start with v, got {tag!r}")
    return release_version.is_prerelease(tag[1:])


def download(tag, repo, name, directory):
    run("gh", "release", "download", tag, "--repo", repo, "--pattern", name, "--dir", str(directory))
    return directory / name


def find_release(repo, tag):
    # The tags endpoint omits drafts; the release list includes them.
    pages = json.loads(run("gh", "api", "--paginate", "--slurp", f"repos/{repo}/releases", capture=True))
    matches = [release for page in pages for release in page if release["tag_name"] == tag]
    if len(matches) != 1:
        raise SystemExit(f"expected one release for {tag}, found {len(matches)}")
    return matches[0]


def resolve_tag_commit(repo, tag):
    """Resolve the remote git tag to a 40-hex commit SHA, peeling annotated tags."""
    try:
        ref = json.loads(run("gh", "api", f"repos/{repo}/git/ref/tags/{tag}", capture=True))
    except (subprocess.CalledProcessError, json.JSONDecodeError, OSError) as error:
        raise SystemExit(f"refusing to publish {tag}: failed to resolve remote tag commit: {error}") from None

    obj = ref.get("object") if isinstance(ref, dict) else None
    for _ in range(5):
        if not isinstance(obj, dict):
            break
        obj_type = obj.get("type")
        obj_sha = obj.get("sha")
        if not isinstance(obj_sha, str) or not day0_gate._is_hex40(obj_sha):
            break
        if obj_type == "commit":
            return obj_sha
        if obj_type == "tag":
            try:
                tag_obj = json.loads(run("gh", "api", f"repos/{repo}/git/tags/{obj_sha}", capture=True))
            except (subprocess.CalledProcessError, json.JSONDecodeError, OSError) as error:
                raise SystemExit(
                    f"refusing to publish {tag}: failed to peel annotated tag {obj_sha}: {error}"
                ) from None
            obj = tag_obj.get("object") if isinstance(tag_obj, dict) else None
            continue
        break
    raise SystemExit(f"refusing to publish {tag}: remote tag did not resolve to a 40-hex commit SHA")


def verify_publish_day0(
    tag,
    evidence,
    *,
    commit,
    release_assets,
    fetcher=None,
    ledger_path=None,
    source_pin=None,
):
    """Run a fresh day-0 gate + candidate-bound evidence verification before stable publication."""
    if not isinstance(commit, str) or not day0_gate._is_hex40(commit):
        raise SystemExit(f"refusing to publish {tag}: candidate commit SHA is required and must be 40-hex, got {commit!r}")
    if not isinstance(release_assets, dict) or not release_assets:
        raise SystemExit(f"refusing to publish {tag}: draft release has no assets to bind against evidence manifest")
    kwargs = {
        "tag": tag,
        "commit": commit,
        "evidence_path": evidence,
        "require_evidence": True,
        "expected_release_assets": release_assets,
        "source_pin": source_pin,
    }
    if fetcher is not None:
        kwargs["fetcher"] = fetcher
    if ledger_path is not None:
        kwargs["ledger_path"] = ledger_path
    result = day0_gate.run_day0_gate(**kwargs)
    if not result.passed:
        details = "\n".join(f"  - {error}" for error in result.errors)
        raise SystemExit(f"refusing to publish {tag}: day-0 gate failed:\n{details}")
    return result


def main(argv=None, *, fetcher=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--tag", required=True)
    parser.add_argument("--repo", default="18-lab-dev/vosu")
    parser.add_argument("--key", default=RELEASE_KEY)
    parser.add_argument("--publish", action="store_true", help="publish the draft release after signing")
    parser.add_argument(
        "--evidence",
        type=Path,
        default=None,
        help="candidate-bound release evidence manifest JSON (required for --publish on stable >= 2.0.0 releases)",
    )
    parser.add_argument(
        "--commit",
        default=None,
        help="expected 40-hex candidate commit SHA (defaults to release target_commitish when 40-hex)",
    )
    args = parser.parse_args(argv)

    # Before any download, signature upload or publication.
    try:
        prerelease = is_prerelease_tag(args.tag)
        needs_day0 = day0_gate.requires_day0_gate(args.tag)
    except ValueError as error:
        raise SystemExit(f"error: {error}") from None

    if args.publish and needs_day0:
        if args.evidence is None:
            raise SystemExit(
                f"error: --evidence <path> is required when publishing stable release {args.tag}"
            )
        if args.commit is not None and not day0_gate._is_hex40(args.commit):
            raise SystemExit(f"error: --commit must be a 40-hex SHA, got {args.commit!r}")

    release = find_release(args.repo, args.tag)
    names = {asset["name"] for asset in release["assets"]}
    asset_digests = {}
    with tempfile.TemporaryDirectory(prefix="release-sign-") as directory:
        root = Path(directory)
        signatures = []
        verified = []

        for asset in release["assets"]:
            if asset["name"].endswith(".sig"):
                continue
            digest = asset.get("digest") or ""
            if not digest.startswith("sha256:"):
                raise SystemExit(f"{asset['name']}: GitHub recorded no SHA-256 digest")
            expected_sha = digest.removeprefix("sha256:")
            path = download(args.tag, args.repo, asset["name"], root)
            if sha256(path) != expected_sha:
                raise SystemExit(f"{asset['name']}: download does not match GitHub's digest")
            asset_digests[asset["name"]] = expected_sha
            if path.name + ".sig" in names:
                existing = download(args.tag, args.repo, path.name + ".sig", root)
                require_signed_by(path, existing, args.key)
                verified.append(path.name)
            else:
                signatures.append(sign(path, args.key))
        if signatures:
            run("gh", "release", "upload", args.tag, "--repo", args.repo, *map(str, signatures))
        for signature in signatures:
            print(f"uploaded {signature.name}")
        for name in verified:
            print(f"verified existing signature for {name}")
    if args.publish and release["draft"]:
        if needs_day0:
            # Re-verify day-0 readiness and the candidate-bound evidence manifest
            # immediately before publication so a stale CI draft can never bypass
            # the gate after creation. Commit and asset binding fail closed.
            if not asset_digests:
                raise SystemExit(
                    f"refusing to publish {args.tag}: draft release has no assets to bind against evidence manifest"
                )
            target_commit = args.commit
            if target_commit is None:
                candidate_commitish = release.get("target_commitish")
                if isinstance(candidate_commitish, str) and day0_gate._is_hex40(candidate_commitish):
                    target_commit = candidate_commitish
                else:
                    target_commit = resolve_tag_commit(args.repo, args.tag)
            verify_publish_day0(
                args.tag,
                args.evidence,
                commit=target_commit,
                release_assets=asset_digests,
                fetcher=fetcher,
            )
        fields = ["-F", "draft=false"]
        if prerelease:
            # A prerelease tag must not become the repo's "Latest release" or
            # be marked stable: downstream updaters and release pages key off
            # both flags to decide what to offer as a general-availability build.
            fields += ["-F", "prerelease=true", "-F", "make_latest=false"]
        run("gh", "api", "--method", "PATCH", f"repos/{args.repo}/releases/{release['id']}",
            *fields, capture=True)
        print(f"published {args.tag}")


if __name__ == "__main__":
    main()
