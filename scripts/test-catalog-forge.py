#!/usr/bin/env python3
"""D15 internal-CA real-forge gate for catalog sources (M6b X1).

Linux (default): provisions a pinned Forgejo on an isolated (`--internal`)
container network, generates a throwaway internal CA with exact-hostname
leaves, then runs the `network_integration` test binary's ignored `forge::`
and `os_store::` tests inside a container whose own OS trust store has the CA
installed (update-ca-certificates), and again in the same image without it.
The test process serves the forge's TLS front end under the network aliases
`forge.vosu-x1.test`, `mirror.vosu-x1.test` and `wrong.vosu-x1.test`.

    source scripts/dev-env.sh
    export CARGO_TARGET_DIR=...
    python scripts/test-catalog-forge.py [--engine podman|docker]

Missing prerequisites (container engine, image pull, openssl, cargo) fail the
run; nothing is skipped and an unexpected test count fails. Every container
and network created here is named `vosu-x1-<run>-*` and is removed in
`finally`, including on failure or SIGTERM.

Native Windows (`--windows-vm`, run by the integrator with the VM running):
adds the fixture CA to the VM's LocalMachine `Root` store, runs the
`os_store::trusted::` phase through scripts/vm-test.py, removes the CA, then
runs `os_store::untrusted::`. The CA is removed in `finally`.

No real forge, token or system trust store of this machine is touched.
"""

import argparse
import base64
import hashlib
import io
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parent.parent

# Pinned by OCI index digest; the tag is informational.
FORGE_IMAGE = (
    "codeberg.org/forgejo/forgejo:17.0.2-rootless"
    "@sha256:39362ab93a94c0c9b8b330d2a877ddbb3f02fa8c0cba1b8d686d437b39f36a04"
)
FORGE_VERSION_PREFIX = "17.0.2"
FORGE_PORT = 3000
ENGINE_ENV = "VOSU_X1_CONTAINER_ENGINE"

FORGE_HOST = "forge.vosu-x1.test"
MIRROR_HOST = "mirror.vosu-x1.test"
WRONG_HOST = "wrong.vosu-x1.test"
HTTPS_PORT = 443
CROSS_PORT = 8443

# Shared contract with crates/vosu-test-harness/tests/network_integration/fixture.rs.
OWNER = "vosu-x1-owner"
PRIVATE_REPO, PRIVATE_TAG = "private-catalog", "v1.0.0"
SAME_ORIGIN_REPO, SAME_ORIGIN_TAG = "same-origin-redirect", "v2.0.0"
CROSS_PORT_REPO, CROSS_PORT_TAG = "cross-port-redirect", "v3.0.0"
FOREIGN_REDIRECT_REPO, FOREIGN_REDIRECT_TAG = "foreign-redirect", "v4.0.0"
FOREIGN_ASSET_REPO, FOREIGN_ASSET_TAG = "foreign-asset", "v5.0.0"
CATALOG_ASSET = "catalog-v2.json"
HOP_ASSET = "hop.json"
PACKAGE_ASSET = "x1-package.zip"
ICON_ASSET = "x1-icon.png"
TOOL_ID = "x1-private-tool"
TOOL_VERSION = "3.1.4"

TEST_BINARY = "network_integration"
BUILD_CMD = [
    "cargo", "test", "--locked", "--workspace", "--all-features", "--test", TEST_BINARY,
    "--no-run", "--message-format=json-render-diagnostics",
]
EXPECTED_TRUSTED = (
    "forge::trusted::foreign_hosts_are_denied_at_redirect_and_initial_asset",
    "forge::trusted::package_and_art_on_the_private_source_host_stay_public_and_credential_free",
    "forge::trusted::private_source_is_refused_by_default_then_fetched_with_its_exact_flag",
    "forge::trusted::production_client_trusts_the_installed_ca_for_the_exact_forge_name_only",
    "forge::trusted::real_forge_cross_port_redirect_strips_the_credential_for_the_whole_chain",
    "forge::trusted::real_forge_same_origin_redirect_keeps_the_credential",
    "forge::trusted::rejected_token_is_reported_without_echo",
    "forge::trusted::static_basic_credential_is_received_and_kept_out_of_storage",
    "os_store::trusted::installed_ca_trusts_the_exact_fixture_name_only",
)
EXPECTED_UNTRUSTED = (
    "forge::untrusted::missing_ca_rejects_the_real_forge_before_any_request_or_credential",
    "os_store::untrusted::missing_ca_rejects_the_fixture_leaf_before_any_request",
)
PHASES = {
    "trusted": (("forge::trusted::", "os_store::trusted::"), EXPECTED_TRUSTED),
    "untrusted": (("forge::untrusted::", "os_store::untrusted::"), EXPECTED_UNTRUSTED),
}
WINDOWS_PHASES = {
    "trusted": (("os_store::trusted::",), tuple(n for n in EXPECTED_TRUSTED if n.startswith("os_store::"))),
    "untrusted": (("os_store::untrusted::",), tuple(n for n in EXPECTED_UNTRUSTED if n.startswith("os_store::"))),
}
CA_STORE_PATH = "/usr/local/share/ca-certificates/vosu-x1-fixture.crt"


class GateError(RuntimeError):
    """A prerequisite or result failure; the gate fails, never skips."""


def download_path(repo, tag, asset):
    return f"/{OWNER}/{repo}/releases/download/{tag}/{asset}"


def forge_url(path, port=HTTPS_PORT):
    return f"https://{FORGE_HOST}{'' if port == 443 else f':{port}'}{path}"


# ---------------------------------------------------------------------------
# Pure helpers (covered by scripts/test_catalog_forge.py).
# ---------------------------------------------------------------------------


def release_plan():
    """Repositories, latest releases and assets provisioned in Forgejo.

    External assets make Forgejo itself answer `releases/download/...` with a
    303 to their URL, which is how the redirect cases use the real forge's own
    redirect instead of a mocked one."""
    hop = lambda repo, tag: forge_url(download_path(repo, tag, HOP_ASSET))  # noqa: E731
    private_catalog = forge_url(download_path(PRIVATE_REPO, PRIVATE_TAG, CATALOG_ASSET))
    mirror = f"https://{MIRROR_HOST}/x1/mirror/{CATALOG_ASSET}"
    return [
        (PRIVATE_REPO, PRIVATE_TAG, [
            (CATALOG_ASSET, "file", "catalog-v2.json"),
            (PACKAGE_ASSET, "file", PACKAGE_ASSET),
        ]),
        (SAME_ORIGIN_REPO, SAME_ORIGIN_TAG, [
            (CATALOG_ASSET, "external", hop(SAME_ORIGIN_REPO, SAME_ORIGIN_TAG)),
            (HOP_ASSET, "external", private_catalog),
        ]),
        (CROSS_PORT_REPO, CROSS_PORT_TAG, [
            (CATALOG_ASSET, "external", hop(CROSS_PORT_REPO, CROSS_PORT_TAG)),
            (HOP_ASSET, "external", forge_url("/x1/return" + download_path(PRIVATE_REPO, PRIVATE_TAG, CATALOG_ASSET), CROSS_PORT)),
        ]),
        (FOREIGN_REDIRECT_REPO, FOREIGN_REDIRECT_TAG, [
            (CATALOG_ASSET, "external", hop(FOREIGN_REDIRECT_REPO, FOREIGN_REDIRECT_TAG)),
            (HOP_ASSET, "external", mirror),
        ]),
        (FOREIGN_ASSET_REPO, FOREIGN_ASSET_TAG, [
            (CATALOG_ASSET, "external", mirror),
        ]),
    ]


def catalog_document(package):
    return {
        "catalog_version": 2,
        "name": "vosu-x1 private catalog",
        "published": "2026-11-24T00:00:00Z",
        "tools": [{
            "id": TOOL_ID,
            "name": "X1 private tool",
            "version": TOOL_VERSION,
            "host": "^2.0.0",
            "icon_url": forge_url(download_path(PRIVATE_REPO, PRIVATE_TAG, ICON_ASSET)),
            "packages": {"any": {
                "url": forge_url(download_path(PRIVATE_REPO, PRIVATE_TAG, PACKAGE_ASSET)),
                "sha256": hashlib.sha256(package).hexdigest(),
                "size": len(package),
            }},
        }],
    }


def package_bytes(run_id):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as archive:
        info = zipfile.ZipInfo("x1-tool/README.txt", date_time=(2026, 11, 24, 0, 0, 0))
        archive.writestr(info, f"vosu-x1 package {run_id}\n")
    return buffer.getvalue()


def names(run_id):
    prefix = f"vosu-x1-{run_id}"
    return {
        "network": f"{prefix}-net",
        "forge": f"{prefix}-forge",
        "provision": f"{prefix}-provision",
        "trusted": f"{prefix}-trusted",
        "untrusted": f"{prefix}-untrusted",
    }


def phase_command(engine, image, resources, phase, fixture_dir, binary):
    """Container argv for one phase; the CA mount is the only difference."""
    filters, _ = PHASES[phase]
    argv = [
        engine, "run", "--rm", "--name", resources[phase],
        "--network", resources["network"],
        "--network-alias", FORGE_HOST,
        "--network-alias", MIRROR_HOST,
        "--network-alias", WRONG_HOST,
        "--user", "root",
        "-v", f"{fixture_dir}:/x1/fixture:ro",
        "-v", f"{binary}:/x1/{TEST_BINARY}:ro",
        "-e", "VOSU_X1_FIXTURE=/x1/fixture/fixture.json",
    ]
    if phase == "trusted":
        argv += ["-v", f"{fixture_dir / 'ca.crt'}:{CA_STORE_PATH}:ro"]
    script = (
        "update-ca-certificates --fresh >/dev/null 2>&1 && "
        f"exec /x1/{TEST_BINARY} --ignored --test-threads=1 {' '.join(filters)}"
    )
    return argv + [image, "sh", "-c", script]


def listed_tests(output):
    return sorted(line[: -len(": test")] for line in output.splitlines() if line.endswith(": test"))


def check_results(output, expected):
    """Exact names and counts; zero tests or any drift fails."""
    passed = sorted(re.findall(r"^test (\S+) \.\.\. ok$", output, re.MULTILINE))
    summary = re.findall(r"^test result: ok\. (\d+) passed; 0 failed; 0 ignored;", output, re.MULTILINE)
    if not expected:
        raise GateError("no expected tests: refusing a zero-test pass")
    if passed != sorted(expected) or summary != [str(len(expected))]:
        raise GateError(
            f"expected exactly {len(expected)} passing tests {sorted(expected)}, "
            f"saw passed={passed} summary={summary}"
        )
    return len(passed)


def find_secrets(text, sentinels):
    return [name for name, value in sentinels.items() if value and value in text]


def redact(text, sentinels):
    for name, value in sentinels.items():
        if value:
            text = text.replace(value, f"<redacted {name}>")
    return text


# ---------------------------------------------------------------------------
# In-container provisioning (stdlib only; runs on the isolated network).
# ---------------------------------------------------------------------------


def _request(base, method, path, auth=None, payload=None, fields=None, files=None):
    headers = {"Accept": "application/json"}
    data = None
    if auth:
        headers["Authorization"] = auth
    if payload is not None:
        data = json.dumps(payload).encode()
        headers["Content-Type"] = "application/json"
    elif fields is not None or files is not None:
        boundary = secrets.token_hex(16)
        parts = []
        for name, value in (fields or {}).items():
            parts.append(f'--{boundary}\r\nContent-Disposition: form-data; name="{name}"\r\n\r\n{value}\r\n'.encode())
        for name, (filename, content) in (files or {}).items():
            parts.append(
                f'--{boundary}\r\nContent-Disposition: form-data; name="{name}"; filename="{filename}"\r\n'
                "Content-Type: application/octet-stream\r\n\r\n".encode() + content + b"\r\n"
            )
        data = b"".join(parts) + f"--{boundary}--\r\n".encode()
        headers["Content-Type"] = f"multipart/form-data; boundary={boundary}"
    request = urllib.request.Request(base + path, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            body = response.read()
            return response.status, json.loads(body) if body else None
    except urllib.error.HTTPError as error:
        return error.code, None


def _basic(user, password):
    return "Basic " + base64.b64encode(f"{user}:{password}".encode()).decode()


def provision_inside(config_path):
    config = json.loads(Path(config_path).read_text(encoding="utf-8"))
    base = config["api"]
    fixture = Path(config_path).parent
    owner = _basic(OWNER, config["owner_password"])
    reader = config["reader"]

    def must(expected, result, what):
        status, body = result
        if status != expected:
            raise GateError(f"provisioning {what}: HTTP {status}")
        return body

    for repo, tag, assets in release_plan():
        must(201, _request(base, "POST", "/user/repos", owner, {
            "name": repo, "private": True, "auto_init": True, "default_branch": "main",
        }), f"repo {repo}")
        must(204, _request(base, "PUT", f"/repos/{OWNER}/{repo}/collaborators/{reader}", owner,
                           {"permission": "read"}), f"collaborator {repo}")
        release = must(201, _request(base, "POST", f"/repos/{OWNER}/{repo}/releases", owner, {
            "tag_name": tag, "target_commitish": "main", "name": tag,
        }), f"release {repo}")
        for name, kind, value in assets:
            path = f"/repos/{OWNER}/{repo}/releases/{release['id']}/assets"
            if kind == "file":
                result = _request(base, "POST", f"{path}?name={name}", owner,
                                  files={"attachment": (name, (fixture / value).read_bytes())})
            else:
                result = _request(base, "POST", path, owner, fields={"name": name, "external_url": value})
            must(201, result, f"asset {repo}/{name}")

    token = must(201, _request(base, "POST", f"/users/{reader}/tokens", _basic(reader, config["reader_password"]), {
        "name": "vosu-x1-read", "scopes": ["read:repository"],
    }), "read-only token")["sha1"]
    latest = f"/repos/{OWNER}/{PRIVATE_REPO}/releases/latest"
    # Preconditions the tests rely on: private without auth, readable with the
    # token, and the token is read-only.
    must(404, _request(base, "GET", latest), "anonymous private release")
    must(200, _request(base, "GET", latest, f"token {token}"), "token release read")
    must(403, _request(base, "POST", f"/repos/{OWNER}/{PRIVATE_REPO}/releases", f"token {token}",
                       {"tag_name": "x1-write-probe", "target_commitish": "main"}), "read-only token write")
    version = must(200, _request(base, "GET", "/version"), "version")["version"]
    print(json.dumps({"token": token, "version": version}))


# ---------------------------------------------------------------------------
# Host orchestration.
# ---------------------------------------------------------------------------


def run(cmd, *, check=True, timeout=600, env=None, quiet=False):
    completed = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, check=False, env=env)
    if check and completed.returncode != 0:
        detail = "" if quiet else f"\n{completed.stdout[-4000:]}\n{completed.stderr[-4000:]}"
        raise GateError(f"command failed ({completed.returncode}): {' '.join(map(str, cmd[:4]))} ...{detail}")
    return completed


def require(binary, why):
    path = shutil.which(binary)
    if path is None:
        raise GateError(f"{binary} not found on PATH ({why}); this gate fails rather than skipping")
    return path


def ensure_image(engine, reference, build=None):
    if run([engine, "image", "inspect", reference], check=False, timeout=60).returncode == 0:
        return
    if build is None:
        result = run([engine, "pull", reference], check=False, timeout=1200)
    else:
        result = run([engine, "build", "-t", reference, *build], check=False, timeout=1800)
    if result.returncode != 0:
        raise GateError(f"image {reference} is unavailable: {result.stderr[-2000:]}")


def runner_image(engine):
    import importlib.util

    spec = importlib.util.spec_from_file_location("ci_container_run_tests", ROOT / "scripts/ci-container/run_tests.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    tag = module.image_tag()
    ensure_image(engine, tag, ["-f", str(module.CONTAINERFILE), str(module.CONTAINER_DIR)])
    return tag


def generate_pki(directory, run_id):
    """Throwaway CA plus one leaf per exact hostname (forge, mirror)."""
    openssl = require("openssl", "fixture CA generation")
    ec = ["-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1", "-nodes"]
    run([openssl, "req", "-x509", *ec, "-keyout", str(directory / "ca.key"), "-out", str(directory / "ca.crt"),
         "-days", "2", "-subj", f"/CN=vosu-x1 fixture CA {run_id}",
         "-addext", "basicConstraints=critical,CA:TRUE,pathlen:0",
         "-addext", "keyUsage=critical,keyCertSign,cRLSign",
         "-addext", "subjectKeyIdentifier=hash"])
    for name, host in (("forge", FORGE_HOST), ("mirror", MIRROR_HOST)):
        extensions = directory / f"{name}.ext"
        extensions.write_text(
            "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\n"
            f"extendedKeyUsage=serverAuth\nsubjectAltName=DNS:{host}\n"
            "subjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid\n",
            encoding="utf-8",
        )
        run([openssl, "req", *ec, "-keyout", str(directory / f"{name}.key"), "-out", str(directory / f"{name}.csr"),
             "-subj", f"/CN={host}"])
        run([openssl, "x509", "-req", "-in", str(directory / f"{name}.csr"), "-CA", str(directory / "ca.crt"),
             "-CAkey", str(directory / "ca.key"), "-set_serial", f"0x{secrets.token_hex(8)}", "-days", "2",
             "-out", str(directory / f"{name}.crt"), "-extfile", str(extensions)])
    # The CA key never leaves this temporary directory; nothing trusts it
    # after the run.
    (directory / "ca.key").unlink()


def fixture_json(forge=None):
    return {
        "hosts": {"forge": FORGE_HOST, "mirror": MIRROR_HOST, "wrong": WRONG_HOST},
        "ca_cert": "ca.crt",
        "forge_leaf": {"cert": "forge.crt", "key": "forge.key"},
        "mirror_leaf": {"cert": "mirror.crt", "key": "mirror.key"},
        "forge": forge,
    }


def build_binary():
    require("cargo", "test binary build")
    result = run(BUILD_CMD, timeout=3600)
    for line in result.stdout.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if message.get("reason") == "compiler-artifact" and message.get("target", {}).get("name") == TEST_BINARY \
                and message.get("executable"):
            return Path(message["executable"])
    raise GateError(f"cargo produced no {TEST_BINARY} executable")


def verify_listing(binary):
    listed = [name for name in listed_tests(run([str(binary), "--list", "--ignored"]).stdout)
              if name.startswith(("forge::", "os_store::"))]
    expected = sorted(EXPECTED_TRUSTED + EXPECTED_UNTRUSTED)
    if listed != expected:
        raise GateError(f"ignored D15 tests drifted from this runner: listed={listed} expected={expected}")


def wait_ready(engine, forge, deadline_seconds=180):
    deadline = time.monotonic() + deadline_seconds
    while time.monotonic() < deadline:
        probe = run([engine, "exec", forge, "curl", "-fsS", f"http://127.0.0.1:{FORGE_PORT}/api/v1/version"],
                    check=False, timeout=30)
        if probe.returncode == 0 and FORGE_VERSION_PREFIX in probe.stdout:
            return probe.stdout.strip()
        time.sleep(1)
    raise GateError(f"Forgejo did not become ready within {deadline_seconds}s")


def cleanup(engine, resources):
    for key in ("trusted", "untrusted", "provision", "forge"):
        run([engine, "rm", "-f", resources[key]], check=False, timeout=120)
    run([engine, "network", "rm", "-f", resources["network"]], check=False, timeout=120)
    prefix = resources["network"][: -len("-net")]
    left = run([engine, "ps", "-a", "--filter", f"name={prefix}", "--format", "{{.Names}}"], check=False).stdout.split()
    networks = [n for n in run([engine, "network", "ls", "--format", "{{.Name}}"], check=False).stdout.split()
                if n.startswith(prefix)]
    volumes = [v for v in run([engine, "volume", "ls", "--format", "{{.Name}}"], check=False).stdout.split()
               if v.startswith(prefix)]
    return {"containers": left, "networks": networks, "volumes": volumes}


def linux_gate(args, run_id, artifacts):
    engine = require(args.engine, "isolated forge containers")
    run([engine, "info"], timeout=120)
    resources = names(run_id)
    summary = {"run": run_id, "engine": args.engine, "forge_image": FORGE_IMAGE, "phases": {}}
    sentinels = {}
    failure = None
    with tempfile.TemporaryDirectory(prefix="vosu-x1-") as temporary:
        fixture = Path(temporary) / "fixture"
        fixture.mkdir(mode=0o700)
        try:
            ensure_image(engine, FORGE_IMAGE)
            image = runner_image(engine)
            summary["runner_image"] = image
            binary = build_binary()
            verify_listing(binary)

            generate_pki(fixture, run_id)
            package = package_bytes(run_id)
            (fixture / PACKAGE_ASSET).write_bytes(package)
            (fixture / CATALOG_ASSET).write_text(json.dumps(catalog_document(package)), encoding="utf-8")
            owner_password = secrets.token_urlsafe(24)
            reader = f"x1reader{secrets.token_hex(4)}"
            reader_password = f"x1pw-{secrets.token_urlsafe(24)}"
            sentinels.update(username=reader, password=reader_password, owner=owner_password)

            run([engine, "network", "create", "--internal", resources["network"]])
            run([engine, "run", "-d", "--name", resources["forge"], "--network", resources["network"],
                 "-e", "FORGEJO__database__DB_TYPE=sqlite3",
                 "-e", "FORGEJO__security__INSTALL_LOCK=true",
                 "-e", f"FORGEJO__server__ROOT_URL=https://{FORGE_HOST}/",
                 "-e", f"FORGEJO__server__DOMAIN={FORGE_HOST}",
                 "-e", f"FORGEJO__server__HTTP_PORT={FORGE_PORT}",
                 "-e", "FORGEJO__server__DISABLE_SSH=true",
                 "-e", "FORGEJO__server__START_SSH_SERVER=false",
                 "-e", "FORGEJO__server__OFFLINE_MODE=true",
                 "-e", "FORGEJO__service__DISABLE_REGISTRATION=true",
                 FORGE_IMAGE])
            summary["forge_ready"] = wait_ready(engine, resources["forge"])
            for user, password, admin in ((OWNER, owner_password, True), (reader, reader_password, False)):
                run([engine, "exec", resources["forge"], "forgejo", "admin", "user", "create",
                     "--username", user, "--password", password, "--email", f"{user}@vosu-x1.test",
                     "--must-change-password=false", *(["--admin"] if admin else [])], quiet=True)

            (fixture / "provision.json").write_text(json.dumps({
                "api": f"http://{resources['forge']}:{FORGE_PORT}/api/v1",
                "owner_password": owner_password, "reader": reader, "reader_password": reader_password,
            }), encoding="utf-8")
            # Root inside the container is the invoking user under rootless
            # podman, which can read the 0700 fixture directory.
            provisioned = run([engine, "run", "--rm", "--name", resources["provision"], "--user", "root",
                               "--network", resources["network"],
                               "-v", f"{fixture}:/x1/fixture:ro",
                               "-v", f"{Path(__file__).resolve()}:/x1/test-catalog-forge.py:ro",
                               image, "python3", "/x1/test-catalog-forge.py",
                               "--provision-inside", "/x1/fixture/provision.json"], check=False)
            if provisioned.returncode != 0:
                # stdout carries the token only on success; stderr is redacted.
                raise GateError(f"provisioning failed: {redact(provisioned.stderr[-4000:], sentinels)}")
            result = json.loads(provisioned.stdout.strip().splitlines()[-1])
            sentinels["token"] = result["token"]
            summary["forge_version"] = result["version"]
            (fixture / "provision.json").unlink()
            secrets_file = fixture / "secrets.json"
            secrets_file.write_text(json.dumps({
                "token": result["token"], "username": reader, "password": reader_password,
            }), encoding="utf-8")
            secrets_file.chmod(0o600)
            (fixture / "fixture.json").write_text(json.dumps(fixture_json({
                "upstream": f"http://{resources['forge']}:{FORGE_PORT}",
                "https_port": HTTPS_PORT, "cross_port": CROSS_PORT, "version": result["version"],
                "secrets": "secrets.json", "package": PACKAGE_ASSET,
            })), encoding="utf-8")

            for phase in ("trusted", "untrusted"):
                completed = run(phase_command(engine, image, resources, phase, fixture, binary),
                                check=False, timeout=1200)
                output = completed.stdout + completed.stderr
                leaked = find_secrets(output, sentinels)
                (artifacts / f"{phase}.log").write_text(redact(output, sentinels), encoding="utf-8")
                if leaked:
                    raise GateError(f"{phase} output contains fixture secrets {leaked}")
                if completed.returncode != 0:
                    raise GateError(f"{phase} phase failed (exit {completed.returncode}); see {artifacts / (phase + '.log')}")
                summary["phases"][phase] = {
                    "filters": PHASES[phase][0],
                    "passed": check_results(completed.stdout, PHASES[phase][1]),
                    "tests": PHASES[phase][1],
                }
        except (GateError, subprocess.TimeoutExpired, OSError, json.JSONDecodeError, KeyError) as error:
            failure = error
        finally:
            logs = run([engine, "logs", resources["forge"]], check=False, timeout=60)
            (artifacts / "forge.log").write_text(redact(logs.stdout + logs.stderr, sentinels), encoding="utf-8")
            summary["cleanup"] = cleanup(engine, resources)
    (artifacts / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    if failure is not None:
        raise GateError(str(failure)) from failure
    if any(summary["cleanup"].values()):
        raise GateError(f"cleanup left resources behind: {summary['cleanup']}")
    return summary


def windows_gate(args, run_id, artifacts):
    """Native Windows OS store: CA in LocalMachine Root only for one phase."""
    require("windows-vm", "native Windows VM access")
    remote = f"V:\\x1\\{run_id}"
    summary = {"run": run_id, "remote": remote, "phases": {}}

    def vm(*command, check=True, timeout=300):
        return run(["windows-vm", *command], check=check, timeout=timeout)

    def in_root_store(thumbprint):
        """True/False from the VM, or None when the VM did not answer."""
        probe = vm("exec", f"Test-Path 'Cert:\\LocalMachine\\Root\\{thumbprint}'", check=False)
        answer = probe.stdout.strip().splitlines()[-1:]
        return {("True",): True, ("False",): False}.get(tuple(answer))

    def remove_ca(thumbprint):
        vm("exec", f"Remove-Item -Path 'Cert:\\LocalMachine\\Root\\{thumbprint}' -ErrorAction SilentlyContinue",
           check=False)

    status = json.loads(vm("status", "--json", timeout=60).stdout)
    if status.get("state") != "running":
        raise GateError("Windows VM is not running; this mode never starts it")
    with tempfile.TemporaryDirectory(prefix="vosu-x1-win-") as temporary:
        bundle = Path(temporary)
        generate_pki(bundle, run_id)
        (bundle / "fixture.json").write_text(json.dumps(fixture_json()), encoding="utf-8")
        thumbprint = run(["openssl", "x509", "-in", str(bundle / "ca.crt"), "-noout", "-fingerprint", "-sha1"]
                         ).stdout.split("=", 1)[1].strip().replace(":", "")
        summary["ca_sha1"] = thumbprint
        installed = False
        try:
            vm("exec", f"New-Item -ItemType Directory -Force -Path '{remote}' | Out-Null")
            for name in ("fixture.json", "ca.crt", "forge.crt", "forge.key", "mirror.crt", "mirror.key"):
                vm("put", str(bundle / name), f"{remote}/{name}")
            for phase in ("trusted", "untrusted"):
                if phase == "trusted":
                    installed = True
                    vm("exec", f"Import-Certificate -FilePath '{remote}\\ca.crt' "
                               "-CertStoreLocation 'Cert:\\LocalMachine\\Root' -ErrorAction Stop | Out-Null")
                else:
                    remove_ca(thumbprint)
                if in_root_store(thumbprint) != (phase == "trusted"):
                    raise GateError(f"LocalMachine Root store state is wrong for the {phase} phase")
                filters, expected = WINDOWS_PHASES[phase]
                completed = run([sys.executable, str(ROOT / "scripts/vm-test.py"),
                                 "--package", "vosu-test-harness", "--test", TEST_BINARY,
                                 "--env", f"VOSU_X1_FIXTURE={remote}\\fixture.json",
                                 "--", "--ignored", "--test-threads=1", *filters], check=False, timeout=3600)
                output = completed.stdout + completed.stderr
                (artifacts / f"windows-{phase}.log").write_text(output, encoding="utf-8")
                if completed.returncode != 0:
                    raise GateError(f"Windows {phase} phase failed; see {artifacts / f'windows-{phase}.log'}")
                summary["phases"][phase] = {"passed": check_results(completed.stdout, expected), "tests": expected}
        finally:
            if installed:
                remove_ca(thumbprint)
            summary["ca_removed"] = in_root_store(thumbprint) is False
            vm("exec", f"Remove-Item -Recurse -Force '{remote}' -ErrorAction SilentlyContinue", check=False)
    (artifacts / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    if not summary["ca_removed"]:
        raise GateError(f"fixture CA {thumbprint} is still in the VM's Root store")
    return summary


def _terminate(signum, _frame):
    raise KeyboardInterrupt(f"signal {signum}")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--engine", default=os.environ.get(ENGINE_ENV, "podman"), choices=("podman", "docker"),
                        help=f"container engine (default ${ENGINE_ENV} or podman)")
    parser.add_argument("--artifacts", type=Path, help="evidence directory (default <target>/catalog-forge/<run>)")
    parser.add_argument("--windows-vm", action="store_true", help="native Windows LocalMachine Root store phases")
    parser.add_argument("--provision-inside", metavar="CONFIG", help=argparse.SUPPRESS)
    args = parser.parse_args(argv)
    if args.provision_inside:
        provision_inside(args.provision_inside)
        return 0

    signal.signal(signal.SIGTERM, _terminate)
    signal.signal(signal.SIGHUP, _terminate)
    run_id = secrets.token_hex(4)
    target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    artifacts = args.artifacts or target / "catalog-forge" / run_id
    artifacts.mkdir(parents=True, exist_ok=True)
    try:
        summary = (windows_gate if args.windows_vm else linux_gate)(args, run_id, artifacts)
    except (GateError, KeyboardInterrupt) as error:
        print(f"test-catalog-forge: FAILED: {error}\nartifacts: {artifacts}", file=sys.stderr)
        return 1
    for phase, result in summary["phases"].items():
        print(f"{phase}: {result['passed']} passed")
        for name in result["tests"]:
            print(f"  ok {name}")
    print(f"artifacts: {artifacts}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
