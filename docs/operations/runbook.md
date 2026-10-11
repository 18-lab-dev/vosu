---
type: Runbook
title: vosu operations runbook
description: Practical operator notes for building, launching, diagnosing platform access, protecting the local API, handling updates, and understanding release constraints.
resource: README.md
tags: [operations, runbook, security, release, troubleshooting]
---

# Operations runbook

## Documentation maintenance

Update ordinary Markdown in `docs/` in the PR that changes the documented behavior. Review affected contracts and architecture; preserve dated benchmark and experiment evidence rather than rewriting historical results. Documentation maintenance requires no recurring inference or whole-tree generation.

Documentation-only changes still run the documentation-link, provenance, and release-license checks and publish `CI required`; they do not alone require Rust compilation or native Windows verification. Code, package metadata, and workflow changes retain the applicable native gates.

## Launch checklist

- Set up a development checkout with [CONTRIBUTING.md](../../CONTRIBUTING.md); source builds use Rust 1.96.1 and `--locked`.
- Run `cargo run --locked` for local development; Linux process-event backends remain enabled by default.
- Run the daemon as the same desktop user as osu!.
- Confirm `127.0.0.1:24050` is reachable before investigating API readiness.
- Check that the client process is supported and that stable/lazer path and exact-instance offset resolution can complete.
- Keep `OPEN_DASHBOARD_ON_STARTUP` and `ENABLE_AUTOUPDATE` aligned with the intended environment in `vosu.toml`.

A state endpoint returning `500 {"error":"osu is not ready/running"}` means no focused ready instance is currently published; it is not by itself evidence that the server failed.

## Configuration

Edit the platform config root's `vosu.toml`; it is a flat, typed TOML file with uppercase binding names. For example:

```toml
ENABLE_AUTOUPDATE = true
OPEN_DASHBOARD_ON_STARTUP = true
SERVER_IP = "127.0.0.1"
SERVER_PORT = 24050.0
POLL_RATE = 150.0
```

Startup precedence is `vosu.toml > vosu.env > tosu.env > defaults`. The two `.env` files are migration inputs only: vosu writes canonical TOML atomically first and removes readable legacy files only after successful replacement. A malformed TOML file leaves the prior runtime configuration active during watch reload; at startup, malformed input leaves all sources untouched.

The dashboard’s installed cards persist pin state in browser local storage, keep pinned cards first, and reorder pins with **Move pin earlier/later** in each card's **⋯** menu. Shift-click a counter URL to open it in a new window, or press `/` outside an editable control to focus search. If catalog retrieval fails, the Available page can show an error while retaining the locally discovered installed count.

## Platform access

Linux stable uses Wine process discovery and native reads; lazer requires an exposed `osu!.deps.json` layout. `VOSU_OSU_PATH` can override path discovery. If ptrace policy blocks same-user reads, use the distribution-appropriate configuration or narrowly grant `cap_sys_ptrace`; do not run the dashboard as root. Windows requires supported native process combinations and MSVC-built releases.

Offset resolution is automatic per discovered process: a valid versioned
`<version>.json` cache entry wins, then matching embedded offsets, then local
generation from that exact PID and version. Generation never downloads offsets,
scans another process, or uses a local-source override. The extracted helper
cache is separately keyed by platform/runtime and embedded bundle checksum.
The offsets cache is under `$XDG_CACHE_HOME/vosu/offsets` (default
`$HOME/.cache/vosu/offsets`) on Linux and `.cache\offsets` under the portable
Windows executable directory. Release binaries carry the platform-matching
self-contained generator; debug builds without a prepared payload fail
explicitly. End users do not install .NET.

Offset/signature resolution may remain pending and retry while a process is alive. A pending resolver alone does not confirm that a game build is supported.

## Security boundaries

Keep the listener loopback-only unless remote access is deliberate and protected. Read authorization combines allowed peer/network patterns with allowed `Origin`/`Referer` tokens, retaining headerless/opaque overlay reads; a hostname token does not authorize an arbitrary remote peer. Management POSTs additionally require exact same-origin browser provenance (scheme, host, port), not merely a trusted hostname or loopback peer. Settings sockets retain reads but require that same provenance for writes; use the hosted settings page for file-origin overlays. See the README for method cutover and reverse-proxy boundaries. Preserve request/frame limits, root-confined files, escaped reports, HTTPS-only downloads, private/reserved destination rejection, ZIP safety checks, and same-filesystem replacement.

### Security hardening and remaining trust

The October 2026 hardening preserves existing dashboard/counter URLs and settings protocols. It is not a claim that every finding in the partial security scan is resolved.

- **Native processes:** Linux discovery and opening require the daemon's real UID; Windows opening compares primary-token user SIDs. Linux reads check the captured process generation before and after access. Dependency-file inspection is bounded and nonblocking for Linux FIFOs. This is not a sandbox for same-account game installations or the assemblies used by automatic offset generation.
- **Memory decoding:** mod expansion, tournament/chat/leaderboard collections and retained text have explicit budgets. Resource hashes must be empty or ASCII hexadecimal MD5/SHA-256 digests; hashes are validated before filesystem path slicing. Corrupt or excessive samples are rejected rather than silently trusting advertised collection lengths.
- **Counter installation:** the four-job admission budget, cancellation and join accounting are shared across root generations. Root reloads do not retain an ever-growing list of retired services. Metadata reads validate a regular opened file, reject Unix symlinks/FIFOs and cap content at 64 KiB. ZIP64 preflight rejects mismatched record/locator placement before the decoder can apply its shifted-record fallback.
- **Runtime limits:** intervals are capped at 60 seconds; oversized settings updates reject the entire update. Calculator input is capped at 64 MiB and graph output at 1,000,000 aggregate bins. Catch banana generation stops when f32 timestamp addition cannot advance and caps each object's output at 100,000. Ordinary spacing is retained; unusually long objects may be truncated. This local guard is documented separately from pinned external oracle evidence.
- **Transport and tools:** relay/control queues are bounded, slow relay recipients do not stall the shared worker, and filter depth/node budgets bound projection amplification. Offline comparison validates Git filter/attribute configuration before worktree status can execute it; dense alignment fails explicitly above 1,000,000 retained candidates instead of approximating its ordering semantics.
- **Bounded progress and delivery:** stable hit-error backlogs advance in chunks of at most 4,096 events per capture. Filter node/depth limits retain ordered last-write projection semantics. One-shot settings replies use bounded control handles independent of the receive mutex, so contention cannot drop a refresh or stall healthy recipients.
- **Updater readiness:** only the exact spawned candidate/rollback process receives the one-use readiness secret through its private stdin pipe. The secret is no longer passed in process-visible arguments; the transaction token alone cannot authorize readiness. Native helper replacement/rollback coverage remains required on each supported OS.
  This private readiness protocol is a clean cutover for the first daemon release; the only currently published release asset is the offset-generator bundle. Development binaries built with the older argument-token protocol must be replaced manually rather than relying on an old helper to update across this boundary.

**Still unresolved:** installed counters share management authority with the dashboard; native clients can forge browser provenance. [ADR 0013](../decisions/0013-counters-share-dashboard-origin.md) keeps this shared origin for compatibility and records the evaluated isolation designs; the **Available** page discloses it to users. Memory-derived file roots and automatic generation still rely on a trusted same-account game installation. Counter settings/schema files and total rendered output do not have a complete aggregate budget. Per-input and per-queue caps do not bound all concurrent calculations/connections. Legacy curve/slider expansion, other calculator hot-path complexity and some hit-error classification costs remain outside this patch. Keep loopback binding, normal-user execution and trusted counter/game installations; do not describe these mitigations as full isolation.


## Updates and releases

Startup updates follow stable releases, verify the selected platform archive’s published SHA-256 digest, stage exactly one root executable, and use a crash-recoverable helper/rollback flow. Set `ENABLE_AUTOUPDATE = false` in `vosu.toml` or pass `--update=false` to disable startup installation; release builds on self-updating installs still query the latest-release metadata at every start to report available updates. Use same-origin `POST /api/runUpdates` for the explicit route. Do not run Windows directly from an archive or OneDrive-controlled directory because safe replacement/rollback may be unavailable.

Release packaging requires exact platform archives containing only the executable and `LICENSE`; release smoke tests exercise an extracted binary. Windows additionally signs and verifies the executable through the release workflow; the [code signing policy](../../CODE_SIGNING_POLICY.md) records team roles and the manual SignPath approval step, and must be updated with any change to who signs or how. The updater currently reads latest-release metadata from `shiruviaa/vosu`; confirm this source when diagnosing update failures. See `scripts/package-release.py` and `scripts/verify-windows-release.ps1` for enforcement.

### SignPath signing setup

Signing is off until the repository variable `SIGNPATH_ORGANIZATION_ID` exists. Without it, the Windows job packages the unsigned executable and `scripts/verify-windows-release.ps1` checks only its version resources; the release notes say the executable is unsigned. With it, the job submits `vosu.exe` to that SignPath organization, project `vosu`, signing policy `release-signing`, packages the signed result, and runs the verifier with `-RequireSignature`. The step passes no artifact-configuration slug, so SignPath uses the project's default artifact configuration. The previously configured organization ID is `5b025426-9c79-4f04-87e8-64cf64131d0b`. Prerequisites outside this repository:

1. The repository must be public; SignPath Foundation signs open-source projects only.
2. The SignPath project `vosu` must be connected to `shiruviaa/vosu` as a trusted GitHub build system, so SignPath verifies that each request comes from a run of this repository's workflow (origin verification).
3. The `release-signing` policy must require manual approval by an approver listed in the code signing policy.
4. The default artifact configuration must sign only `vosu.exe` and restrict its product name. The uploaded GitHub artifact is a ZIP with `vosu.exe` at its root:

   ```xml
   <artifact-configuration xmlns="http://signpath.io/artifact-configuration/v1">
     <zip-file>
       <pe-file path="vosu.exe" product-name="vosu">
         <authenticode-sign />
       </pe-file>
     </zip-file>
   </artifact-configuration>
   ```

   `build.rs` sets `ProductName` to `vosu` and `ProductVersion` to the package version, and `scripts/verify-windows-release.ps1` checks both against the tag after signing. A static configuration cannot pin a per-release version, so the version check stays in the workflow.
5. The repository secret `SIGNPATH_API_TOKEN` must belong to a SignPath CI user that can submit to this project, and the variable `SIGNPATH_ORGANIZATION_ID` is added only after everything else here is in place. Once the variable exists, a missing or invalid token fails the tagged release at the signing step rather than publishing an unsigned archive.
6. Every team member must use MFA for GitHub and SignPath.

Before enabling signing, and after any change to the SignPath project, an approver confirms items 2–4 and 6 in SignPath. As of 2026-10-10 none of these SignPath-side settings has been verified from this repository, and v1.0.0 is released unsigned; GitHub 2FA is enabled for both organization members but is not required by the organization. With `-RequireSignature`, `scripts/verify-windows-release.ps1` requires a valid Authenticode signature issued to `SignPath Foundation` with a timestamp countersignature.

### Linux distribution releases

The Linux release and unsigned-development build jobs use Ubuntu 22.04.
`python scripts/package-linux.py check-glibc --binary <vosu>` inspects
`objdump -T` and rejects requirements newer than glibc 2.35. Tagged release jobs
restore no Cargo caches: they build in a clean target directory from freshly
downloaded, `Cargo.lock`-verified crates, so no output from another run can reach
a published or signed binary. Development build caches include the runner
baseline so newer-host objects cannot leak into those artifacts.
Python 3.12 is installed explicitly because the runner defaults to Python 3.10.

After the exact two-entry ZIP passes smoke testing, release CI installs nfpm
2.47.0 from its SHA-256-pinned upstream archive and runs:

```sh
python scripts/package-linux.py build --tag v1.0.0 \
  --binary target/x86_64-unknown-linux-gnu/release/vosu --output dist
```

This stages the 256px PNG outside checksum-covered assets, builds and inspects
Debian, RPM and Arch package file lists/metadata, and renders the AUR bundle
from the Linux ZIP and tagged desktop/icon bytes. The published set is exactly
the two ZIPs, `vosu_<version>_amd64.deb`,
`vosu-<version>-1.x86_64.rpm`, `vosu-<version>-1-x86_64.pkg.tar.zst`,
and `vosu-bin-aur-v<version>.tar.gz`. `package-release.py verify-set` rejects
missing or extra assets. The AUR tarball contains `PKGBUILD`, `.SRCINFO` and
`vosu-bin.install`; extract it, compare `makepkg --printsrcinfo` with `.SRCINFO`,
and publish those files to the separate AUR `vosu-bin` repository.

Before publication, clean Debian 12, Ubuntu 22.04/24.04, Fedora and Arch
containers install the corresponding local package with dependency resolution,
check launcher/icons, and run `vosu --help`. Debian also starts an isolated,
unprivileged daemon and checks HTTP 200. Capability setup is deliberately
nonfatal: post-install and post-upgrade run `/usr/bin/vosu --setup` as root,
printing `/usr/bin/vosu --setup` recovery guidance on failure. `getcap` records
whether the container could grant capabilities. The daemon never runs as root.
The smoke records file capabilities before clearing them only inside the
disposable container: rootless containers can set a capability outside their
bounding set, then refuse to execute that file. A second restricted Debian
install drops `CAP_SETFCAP` and requires the recovery hint without transaction
failure.

Packages do not ship or enable systemd units. Start at login is a per-user
dashboard option. Packaged executables in non-user-writable directories skip
self-updates and must be upgraded with the package manager. The unsigned
development workflow continues to produce ZIPs only, not distro packages.
See [ADR 0010](../decisions/0010-linux-distribution-packages.md).


### Unsigned development artifacts

`.github/workflows/version-build.yml` is independent of required `ci.yml` validation
and tagged `release.yml` publication. It uses only `contents: read`, no secrets,
signing, tags, releases, or updater publication. SignPath remains deferred for
this development path. A successful development build does not imply required CI
has passed.

- **Gate:** pushes to `main` are path-filtered to root `Cargo.toml`. Python 3.11+
  `tomllib` compares actual `[package].version` at the checked-out push tip against
  `github.event.before`, not `HEAD^`, so multi-commit pushes compare net changes.
  Dependency/comment-only changes and version changes reverted within the push
  skip both native jobs. A missing/zero base, failed depth-one base fetch, missing
  manifest, or invalid TOML fails the gate closed; manual `workflow_dispatch`
  explicitly builds the selected ref's current version without a base.
  GitHub's push path-filter limits still apply (including its 300-file diff limit);
  manual dispatch is the recovery path for a missed automatic run.
- **Scheduling:** one parallel Linux/Windows matrix, independent platform failures,
  45-minute build timeouts, and cancellation of obsolete runs on the same ref.
  Gate timeout is five minutes. A newer qualifying push with an unchanged version
  can cancel an older build and then skip; manually dispatch if that artifact is needed.
- **Build/cache:** pinned Rust 1.96.1 and action SHAs; native x86_64 GNU with exactly
  `--no-default-features --features linux-process-events`, native x86_64 MSVC with
  `--no-default-features`, both `--release --locked`. Before Cargo, the workflow
  uses the .NET 8 SDK to prepare the matching embedded helper bundle, including
  after cache restoration. Cargo always runs, including on exact cache hits.
  Download caches share the existing OS/lockfile namespace.
  Compilation caches are separate from CI/tag builds and partitioned by OS,
  architecture, toolchain, target, and feature/profile mode. Weekly plus
  lockfile/manifest generations bound snapshots; same-mode fallback reuses older
  versions without crossing platforms or features. Incremental compilation is off.
  Only the isolated release smoke crate is tested here; required CI owns full suites.
- **Payload:** each Actions artifact contains
  `vosu-unsigned-dev-v<VERSION>-<FULL_SHA>-<TARGET>.zip`, retained for 14 days with
  upload compression disabled (the inner ZIP is already compressed).
  The ZIP carries the root binary, `BUILD-INFO.json` (unsigned status, version,
  commit, target, toolchain, features), root `LICENSE`, `LICENSE-MIT`,
  `LICENSE-APACHE`, `THIRD_PARTY_NOTICES.md`, calculator license texts including
  `crates/vosu-pp/THIRD_PARTY_LICENSES.txt` and both local fork licenses, and the
  historical oracle license referenced by the notices. License paths retain their
  repository layout. `Cargo.lock`, `assets/SHA256SUMS`, and the provenance audit
  accompany them; the manifest describes repository inputs, not archive members.
  The historical oracle executable is not shipped. This is the retained repository
  attribution bundle, not a newly generated license audit of every registry dependency.
- **Smoke:** `scripts/version-build.py package` checks the exact ZIP member set
  and CRCs and extracts it into a fresh `version-smoke/<platform>` directory,
  restoring Linux executable permissions. Nonempty destinations are rejected.
  The existing smoke harness launches that extracted binary with `--update=false`
  and isolated state; on Windows its configuration is beside the extracted binary.
  Upload happens only after the platform's smoke succeeds. Keep development
  binaries outside existing installations and use `--update=false` manually too.

The tagged/updater ZIP remains exactly executable plus root `LICENSE`; the
generator bundle is embedded inside the executable rather than added as an
archive member. The bundle carries its own `LICENSE`, `LICENSE-MIT`,
`LICENSE-APACHE`, `THIRD_PARTY_NOTICES.md`, and manifest. `scripts/package-release.py`
and the tag workflow retain the exact two-entry archive contract.
Windows development binaries have no Authenticode guarantee. Hosted Linux GNU
builds are not portable-musl artifacts: system glibc/libelf/zlib compatibility
still applies. Artifact smoke is not live osu! parity or privileged process-backend
verification on either platform.

Small local gate probes from the repository root (no compilation):

```sh
python scripts/version-build.py detect --event workflow_dispatch
python scripts/version-build.py detect --event push --before "$(git rev-parse HEAD)"
python scripts/version-build.py detect --event push --before 0000000000000000000000000000000000000000
# Supply the actual pre-push SHA to exercise a multi-commit version change:
python scripts/version-build.py detect --event push --before <PRE_PUSH_SHA>
```

Expected results: dispatch `build=true`; same commit `build=false`; zero base
nonzero exit/no builds; pre-push comparison true only for different parsed
versions. A local base object avoids any network fetch.

For dependency-only, multi-commit, and reverted-version push cases without touching
the working checkout, this disposable fixture needs only Python 3.11+ and Git:

```sh
python - <<'PY'
import os
from pathlib import Path
import subprocess
import sys
import tempfile

script = Path("scripts/version-build.py").resolve()
env = dict(os.environ)
env.pop("GITHUB_OUTPUT", None)
with tempfile.TemporaryDirectory() as directory:
    def git(*args):
        return subprocess.check_output(
            ["git", "-c", "user.name=Version fixture",
             "-c", "user.email=fixture@example.invalid", *args],
            cwd=directory, text=True)
    def commit(version, dependency):
        Path(directory, "Cargo.toml").write_text(
            f'[package]\nname="fixture"\nversion="{version}"\n'
            f'[dependencies]\nexample="{dependency}"\n')
        git("add", "Cargo.toml")
        git("commit", "-qm", "fixture")
        return git("rev-parse", "HEAD").strip()
    def detect(base, expected):
        output = subprocess.check_output(
            [sys.executable, str(script), "detect", "--event", "push",
             "--before", base], cwd=directory, env=env, text=True)
        assert f"build={expected}\n" in output, output
        print(output, end="")
    git("init", "-q")
    base = commit("0.3.0", "1")
    commit("0.3.0", "2")
    detect(base, "false")  # Dependency-only root manifest edit.
    commit("0.4.0", "2")
    commit("0.4.0", "3")
    detect(base, "true")   # Version changed before the tip commit.
    commit("0.3.0", "3")
    detect(base, "false")  # Net version change was reverted.
PY
```

After building the native release binary, reproduce packaging and artifact smoke
on Linux (use a fresh destination; Windows uses `--platform windows` and the
extracted `vosu.exe` path in `VOSU_RELEASE_SMOKE_BINARY`):

```sh
python scripts/version-build.py package --platform linux --sha "$(git rev-parse HEAD)" \
  --destination version-smoke/linux
VOSU_RELEASE_SMOKE_BINARY="$PWD/version-smoke/linux/vosu" \
  cargo test --locked --package vosu-release-smoke --test release_smoke -- --ignored --nocapture
```

## Diagnostics

1. Check logs and the effective platform config root's `vosu.toml` path.
2. Confirm listener binding and authorization before debugging memory access.
3. Confirm process discovery, permissions, client architecture, and offset resolution.
4. Use focused compatibility/native tests for code changes rather than relying on compilation.
5. For precise input issues, dry-run navigation and autoplay before real injection; interpret capture artifacts using count deltas and receive-gap fields, not event-list presence alone.

The source implementation and [runtime/configuration workflow](../workflows/runtime-and-config.md) are authoritative for recovery ordering. Security-sensitive changes require the [testing guide](../testing/verification.md) and relevant native platform evidence.
