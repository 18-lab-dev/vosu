<div align="center">

<picture>
  <source media="(prefers-color-scheme: light)" srcset="assets/branding/vosu-banner-light.svg">
  <img src="assets/branding/vosu-banner.svg" alt="vosu by 18-LAB — Native osu! telemetry, counters, and compatible APIs in Rust." width="100%">
</picture>

<p>
  <a href="rust-toolchain.toml"><img alt="Rust 1.96.1" src="https://img.shields.io/badge/Rust-1.96.1-000000?style=flat-square"></a>
  <a href="#platform-support"><img alt="Linux: first-class" src="https://img.shields.io/badge/Linux-first--class-FCC624?style=flat-square"></a>
  <a href="#platform-support"><img alt="Windows: supported" src="https://img.shields.io/badge/Windows-supported-0078D4?style=flat-square"></a>
  <a href="LICENSE"><img alt="License: MIT OR Apache-2.0" src="https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-6e5494?style=flat-square"></a>
</p>

</div>

> A vibe-coded Rust implementation targeting API and behavioral compatibility with upstream [tosu](https://github.com/tosuapp/tosu), with native Linux implementation, measured hot paths, and a repository designed to work well with coding agents.

The daemon is implemented in Rust, with an embedded vanilla HTML, CSS, and JavaScript dashboard. Upstream tosu defines the compatibility target. Retained upstream material, the dashboard and calculator replacement history, and unresolved source-provenance questions are documented in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) and the [provenance audit](docs/provenance-audit.md). Compatibility does not establish source independence or copyright ownership.

`vosu` reads live state from osu!stable and osu!lazer, then serves the dashboard,
counters, reports, and compatible HTTP/WebSocket APIs. It is one native daemon:
no Node.js, Electron, or separately installed .NET/C# runtime is required.

This build does not provide the former Electron/asdf-overlay in-game overlay or an embedded replacement. External overlay software can consume the documented HTTP and WebSocket APIs; [ADR 0008](docs/decisions/0008-external-in-game-overlays.md) records this product boundary.

## Why this build

| | |
| --- | --- |
| **Native runtime** | A single Rust daemon with embedded dashboard assets and offsets. |
| **Compatibility work** | gosuMemory v1, streamCompanion, and vosu v2 HTTP/WebSocket contracts have targeted checks and fixture evidence. Clean strict Windows stable/lazer captures and their exact source generations are recorded in [verification](docs/testing/verification.md#10-release-gate-strict-windows-stable-2026-10-04). |
| **Linux support** | Native osu!lazer has a [clean strict live-parity capture](docs/testing/verification.md#10-release-gate-strict-native-linux-lazer-2026-10-04) at the recorded source/client pins. osu!stable through Wine is experimental and unverified, not verified release support. |
| **Measured performance** | Criterion covers memory scanning, snapshot reads, calculations, serialization, caching, and publication. Results and limits are recorded in the [benchmark guide](docs/testing/benchmarks.md). |
| **Agent-ready repository** | A pinned toolchain, explicit source ownership, runnable checks, architecture notes, and decision records keep automated changes reviewable. Start with [AGENTS.md](AGENTS.md). |

## Install

### Portable archive

The current release is **1.2.0**. Download `vosu-linux-v1.2.0.zip` or `vosu-windows-v1.2.0.zip` from the [v1.2.0 release](https://github.com/18-lab-dev/vosu/releases/tag/v1.2.0), then:

1. Extract the complete archive into a writable directory. Its root contains `vosu` or `vosu.exe`, plus `LICENSE`.
2. On Linux, restore the executable bit if your archive tool removed it: `chmod +x vosu`.
3. Run `./vosu` on Linux or `vosu.exe` on Windows.
4. Open <http://127.0.0.1:24051/>. By default, vosu opens the dashboard after startup. Overlay URLs stay on port **24050**, preserving their browser storage.

Do not run the Windows executable directly from an archive or a OneDrive-controlled directory. The updater cannot provide safe replacement and rollback in those locations. Windows executables are unsigned, so Windows SmartScreen may warn before the first launch.

### Linux packages

Download the package for your distribution from the matching
[GitHub release](https://github.com/18-lab-dev/vosu/releases):

```sh
# Debian 12+ / Ubuntu 22.04+
sudo apt install ./vosu_1.2.0_amd64.deb
# Fedora
sudo dnf install ./vosu-1.2.0-1.x86_64.rpm
# Arch Linux
sudo pacman -U vosu-1.2.0-1-x86_64.pkg.tar.zst
```

Arch users can build the source `vosu-git` package from the AUR once it is
published. The canonical recipe is
[`packaging/aur/PKGBUILD`](packaging/aur/PKGBUILD), which builds the current
`main` branch and needs the .NET 8 SDK at build time for the embedded offset
generator. For a local checkout:

```sh
cd packaging/aur
makepkg -si
```

Linux binaries require glibc **2.35 or newer** (Debian 12+, Ubuntu 22.04+,
current Fedora and Arch). Packages install the desktop launcher and icons,
and automatically grant process-reading capabilities on install and upgrade.
If the host refuses capabilities, installation still succeeds and prints a
hint to run `/usr/bin/vosu --setup` as your desktop user; it asks for
authentication itself. Never run the daemon itself as root.
Launch **vosu** from the application menu or run `vosu` as your desktop user.
Packaged installs do not use the [self-updater](#automatic-updates), and no
apt, dnf, or pacman repository is configured, so a system upgrade does not
pick up new versions. To upgrade, download the newer release package and
install it with the same command; the package manager replaces the old
version. AUR users update `vosu-git` with an AUR helper or by rebuilding it
from the current repository, which picks up the latest `main`. Start at login
remains an opt-in dashboard setting; packages do not ship or enable a systemd
service.

### Verifying downloads

Every release file from v1.0.1 on has a detached OpenPGP signature `<file>.sig`
made with sxlvia's release key, fingerprint
`6F0F 0D30 C09D 3A03 6246  A810 2CB6 524B B54B 2CDA`. The public key is
[`packaging/release-signing-key.asc`](packaging/release-signing-key.asc).

```sh
gpg --import packaging/release-signing-key.asc
gpg --verify vosu-linux-v1.0.1.zip.sig vosu-linux-v1.0.1.zip
```

Check that gpg reports a good signature from that fingerprint. Commits made
after v1.0.1, and later tags, are signed with the same key.

v1.0.2 is the exception: it was published before it could be signed, and the
repository's immutable releases don't accept files added afterwards. Verify its
downloads with GitHub's release attestation instead:

```sh
gh release verify-asset v1.0.2 vosu-linux-v1.0.2.zip --repo 18-lab-dev/vosu
```

### Manual per-user installation

Install into a user-writable location so vosu can update itself without running as root or Administrator. After extracting the release archive, run:

**Linux**

```sh
install -Dm755 vosu "$HOME/.local/bin/vosu"
install -Dm644 LICENSE "$HOME/.local/share/licenses/vosu/LICENSE"
"$HOME/.local/bin/vosu"
```

Add `$HOME/.local/bin` to `PATH` if you want to launch vosu as `vosu`.

**Windows (PowerShell)**

```powershell
$installDir = Join-Path $env:LOCALAPPDATA "Programs\vosu"
New-Item -ItemType Directory -Force $installDir | Out-Null
Copy-Item .\vosu.exe, .\LICENSE $installDir
& "$installDir\vosu.exe"
```

For a later manual upgrade, stop vosu, replace the executable and `LICENSE` with the files from the new complete archive, then restart it.

### Automatic updates

`ENABLE_AUTOUPDATE=true` follows stable `18-lab-dev/vosu` releases. The updater verifies the selected platform archive against its published SHA-256 digest, stages exactly one root executable, and uses a crash-recoverable helper and rollback flow.

`POST /api/runUpdates` starts the same update path from the same-origin dashboard. Set `ENABLE_AUTOUPDATE=false` or launch with `--update=false` to stop startup installation; `--update=true` enables it for one launch. Release builds still check GitHub for the latest release at every start so the dashboard can offer the update. The [privacy policy](PRIVACY.md) lists that and every other network request.

If the current user cannot write the executable's directory, vosu treats the install as externally managed on both Linux and Windows. Startup skips update checking and recovery, **Automatic updates** is disabled with an explanation, and `POST /api/runUpdates` returns the usual updater-error HTTP 200 JSON response: `{"error":"vosu is managed by your package manager; update it there"}`. Update these installs through their package manager instead.

### Uninstall

Quit vosu from its tray icon first. On Linux, turn off **Settings → General → Start at login** if you enabled it; otherwise run `systemctl --user disable vosu.service` and delete `${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/vosu.service`.

- **Portable archive or manual per-user install:** delete the executable and `LICENSE`, or the whole directory you extracted. On Windows that directory also holds the configuration, overlays, settings, logs, and cache, so deleting it removes everything.
- **Linux packages:** `sudo apt remove vosu`, `sudo dnf remove vosu`, or `sudo pacman -R vosu`. Source AUR installs: `sudo pacman -R vosu-git`.
- **Linux user data:** packages and portable installs leave your data in place. Delete `${XDG_CONFIG_HOME:-$HOME/.config}/vosu`, `${XDG_DATA_HOME:-$HOME/.local/share}/vosu`, and `${XDG_CACHE_HOME:-$HOME/.cache}/vosu` to remove configuration, overlays, logs, and caches.

<!--
SignPath signing is hidden until SignPath Foundation approves the project; until
then every Windows release is unsigned. Restore this section once releases are
signed through SignPath.

## Code signing policy

Windows release executables are signed through SignPath once SignPath Foundation approves the project; until then they are unsigned, and each release's notes state which applies. Free code signing provided by [SignPath.io](https://about.signpath.io), certificate by [SignPath Foundation](https://signpath.org). The [code signing policy](CODE_SIGNING_POLICY.md) lists team roles, what is signed, and how each signing request is approved.
-->

## Privacy

vosu has no accounts, telemetry, or analytics. It contacts GitHub for update checks and downloads, and the tosu catalog and overlay download hosts when you browse or install community overlays. The [privacy policy](PRIVACY.md) details each request and the data kept on your computer. The [security and installation disclosure](docs/security.md) describes how vosu reads the game, the Linux capabilities it requests, and everything it installs or changes; report vulnerabilities as described in [SECURITY.md](SECURITY.md).

## Command-line options

The public command flags are standalone actions:

```text
-h, --help
-s, --setup
-V, --version
```

`--help` prints usage and `--version` prints the version. Each public action
must be the executable's only argument; other arguments start the daemon.
On Linux, run `vosu --setup` as the normal desktop user to probe the compiled
process-event backends and install their capabilities. Setup requests
operating-system authentication when needed; do not run the ordinary daemon as
root.

The daemon also accepts repeatable `--tool <directory>` arguments to load trusted development tools directly from a checkout. Manifests and settings reload live; rebuilding a development service restarts it. See the [tool protocol](docs/tools/protocol.md) for Cargo binary fallback and official/user installation.

Offset resolution is automatic. For each discovered osu!lazer process, vosu first
checks a valid cache entry for that exact process version, then the matching
embedded offsets asset, and finally runs the embedded self-contained generator
against that exact PID and version. Generation is local and produces an offsets
blob; the daemon validates and atomically installs it in the normal offsets cache.
It never downloads offsets or scans a different process, and a failed generation
does not replace an existing cache entry.

End users do not install .NET, an SDK, a source checkout, or a runtime override.
The generator payload is embedded in release binaries and is extracted into the
normal per-user cache only when an exact process needs generation. A release
binary must contain its matching generator payload; a debug/development binary
without a prepared payload reports an explicit missing-bundle error rather than
falling back to a remote source.

Generation runs both managed helpers with the discovered client's bundled CLR
and framework files in a disposable working directory. This keeps measurement
on the same runtime as the game, including newer .NET versions and osu!'s CLR
fork. The verified embedded payload supplies the helper code; neither the game
installation nor the extracted payload is modified. Linux links the client
runtime files; Windows hard-links them when possible and copies them otherwise.

Offset caches live under the platform cache root:

- Linux: `$XDG_CACHE_HOME/vosu/offsets`, defaulting to `$HOME/.cache/vosu/offsets`.
- Windows: `.cache\offsets` under the portable executable directory (the
  canonical Windows cache root).

Offset entries are keyed by client version and remain the validated
`<version>.json` contract. The separately extracted generator payload cache is
keyed by platform/runtime and embedded bundle checksum, so a helper from another
release build cannot be reused accidentally.

## Tools

The dashboard's **Tools** page manages installed applications and lists tools from your catalog sources in **Available**, with install, update, and uninstall actions. Official [18-LAB tools and their SDK](https://github.com/18-lab-dev/vosu-tools) live separately; vosu embeds none. Packages are downloaded over HTTPS, checked against catalog SHA-256 and size, and installed atomically, dependencies first. Updating restarts a running service.

[vosulay](https://github.com/18-lab-dev/vosulay) is available as an optional official tool for in-game overlays, supervised as a separate process and managed from Tools. Rendering and game integration stay outside the vosu daemon, and standalone vosulay remains supported. Exiting its tray normally stops the service without automatic restart (including `always` tools); start it again from the dashboard or vosux, or on the next vosu launch.

Open a tool from the dashboard to grant its UI a write session; opening the same URL directly is read-only. Review a third-party tool's author and requested permissions before trusting it: tools run with your OS permissions and are not sandboxed. User tools go under `<vosu data directory>/tools/<id>/` and require dashboard approval. Developers can use `vosu --tool <directory>` for a trusted development override.

Cards show each public interface: UI, SERVICE and CLI. UI/CLI-only native backends start on demand and stop after ten idle minutes; they have no separate service controls. Background services expose Start/Stop/Restart and **Start with vosu** in Details and Settings. The toggle overrides the author's default for your installation; **Use default** restores it. From a terminal use `vosux --autostart <id> on|off|default`. Only enabled, trusted background services start at launch, dependencies first, without delaying overlay serving.

Install from the terminal with `vosux --install <id>`, update with `vosux --update <id>`, and uninstall with `vosux --uninstall <id> --keep-data` or `--remove-data`. The `vosu x` form accepts the same options. **Settings / Catalog sources** lets you add, remove, disable, and reorder `owner/repo[@tag]` references or HTTPS `catalog.json` URLs. Defaults are exactly `18-lab-dev/vosu-tools` and `18-lab-dev/vosulay`; both are removable. On startup, exact former official identities in saved lists and installed-package state migrate to these identities. Untouched old default lists lose the unavailable private overlay source; customized lists retain their other sources, order, and enabled flags. Overlays continue to come from the existing tosu community catalog. Earlier sources win duplicate ids; later entries are shown as shadowed. `vosux --sources` lists the configured sources.

Catalogs may provide both tools and overlays. Only tools from the exact default source identities `18-lab-dev/vosu-tools` and `18-lab-dev/vosulay` are automatically trusted, including identical shorthand re-added by a user; tagged or HTTPS variants and all other sources remain `user` and need dashboard approval. The overlay **Available** page lists source overlays beside the existing community catalog, preserves its third-party disclosure, and offers to install required tools. Overlay updates preserve settings values; removal uses the existing overlay delete action. See the [catalog source contract](docs/tools/protocol.md#catalog-sources-and-installation) for configuration and APIs.
Start with [Create a tool](docs/tools/create-a-tool.md) for a guided path from an
empty directory to a published catalog. Keep the [authoring reference](docs/tools/authoring.md)
and [protocol contract](docs/tools/protocol.md) nearby, or inspect the dependency-free
[Python example](examples/tools/hello-python/).

Run installed tools from a terminal with `vosu x --list` (or `--list --json`),
`vosu x <tool> [ARGS...]`, or `vosu x --open <tool>`. Linux distro packages also
install `vosux -> vosu` beside the executable; Windows ZIPs retain one executable:
use `vosu x` or create your own link. Options apply only before the tool id;
`vosux practice --help` is the practice tool's help. A UI-only tool opens its UI when
invoked without arguments. The daemon must already be running. Tools inherit
terminal stdin/stdout/stderr and return their exit status, without shell expansion.
CLI launch does not start a second daemon. The private native broker credential
authorizes launching only enabled, trusted, valid tools and opening their UIs, plus tool catalog installation/update/uninstall jobs and source listing. Catalog installation never approves a user-source tool automatically.

### Hosted tool routes and security

The dashboard's `/tools` page manages tools; `/api/tools` lists status and metadata, and `/api/tools/<id>/…` manages lifecycle, settings, logs and data. Management POSTs require dashboard provenance and CSRF. The overlay listener redirects `GET /tools` to that page and serves tool UIs at `/tools/<id>/` and declared service APIs at `/tools/<id>/api/…`. The `tools` counter name is reserved.

Declared GET/HEAD/WebSocket read routes are available to allowed overlay peers. Writes require a tool-bound tab session minted by the dashboard's Open action, or the token of a running tool that declares the target as a dependency. Tool settings and trust state live under the config root's `tools/`; durable data and evictable caches use separate per-tool directories. User-installed tools in the data root's `tools/<id>/` need explicit trust approval. Tools run as your OS account, not in a sandbox: permissions disclose intent and gate host context access, but do not restrict filesystem rights. Only approve code you trust. See [security](docs/security.md#hosted-tools).

## Build from source

[CONTRIBUTING.md](CONTRIBUTING.md) has the step-by-step development setup, including per-distribution Linux packages. In short:

- Rust **1.96.1** (pinned in `rust-toolchain.toml`; rustup installs it automatically)
- Linux: a C toolchain, Clang, pkg-config, and libelf, zlib, and libbpf development files for the default `linux-process-events` feature
- Windows: MSVC build tools and the Windows SDK

The dashboard and baseline offsets are vendored, so a debug build needs no Node.js or .NET toolchain:

```sh
cargo build --locked
cargo run --locked
```

A debug binary without a prepared generator bundle runs normally but reports an explicit missing-bundle error if a lazer build needs local offset generation.

Release builds, including `cargo install --path . --locked`, must embed the matching offset-generator bundle. Preparing it requires Python and the .NET 8 SDK:

```sh
python scripts/prepare-offset-generator.py --rid linux-x64  # or win-x64

# Linux
cargo build --release --locked --no-default-features --features linux-process-events --target x86_64-unknown-linux-gnu

# Windows, from Developer PowerShell
cargo build --release --locked --no-default-features --target x86_64-pc-windows-msvc
```

The script restores locked dependencies and publishes the generator and its owned target self-contained. Output stays under `target/offset-generator/<rid>/` as `offset-generator-v1-<rid>.zip` plus a `.zip.sha256` sidecar and is never committed. The ZIP carries `LICENSE`, `LICENSE-MIT`, `LICENSE-APACHE`, `THIRD_PARTY_NOTICES.md`, and its bundle manifest; the release archive itself remains exactly the executable plus root `LICENSE`.

Default Linux builds include `linux-process-events`, so an installed binary can probe eBPF and CN_PROC during `vosu --setup`. Pass `--no-default-features` only for a polling-only binary. The `test-harness` feature is development-only; `build.rs` rejects it in release profiles.

Updating the vendored `assets/offsets.json` is a separate maintainer procedure: see [Reproduce the vendored osu!lazer offsets asset](docs/operations/offsets-asset.md).

### Unsigned version builds

The [Unsigned version builds workflow](.github/workflows/version-build.yml) builds
native Linux GNU and Windows MSVC development ZIPs when a push to `main` changes
the root `Cargo.toml` package version. Dependency-only edits and unchanged versions
skip compilation; manual dispatch builds the selected ref's current version.
These are **unsigned, nonrelease artifacts**, not updater assets or a replacement
for required CI. Downloads identify the version and full commit, expire after
14 days, and include repository attribution/license texts and build provenance.
Run extracted development binaries with `--update=false`. See the
[operations runbook](docs/operations/runbook.md#unsigned-development-artifacts)
for gating, cache, packaging, and smoke-test details. Tagged publication is
unchanged; this workflow does not sign or publish releases.

## Platform support

Run vosu as the same desktop user as osu!. Tournament and debug-client processes are filtered by command line.

Discovery admits only processes owned by the daemon's desktop account: the real UID on Linux and the primary-token user SID on Windows. Linux memory handles also check the captured process generation once, after each native read; PID reuse is not a new authorized target. Run separate daemon instances for separate desktop accounts.

| Platform | osu!stable | osu!lazer |
| --- | --- | --- |
| **Linux** | **Experimental; not release-verified.** Wine process discovery and native memory reads exist. Set `VOSU_OSU_PATH=/absolute/path/to/osu!` if Wine does not expose the install directory. | The official executable or AppImage layout must expose `osu!.deps.json`. Sandbox policy must allow process discovery and memory reads. |
| **Windows** | Supported 32-bit processes are read through native Windows process-memory APIs. | A supported 64-bit process is required. Offsets are resolved from the valid versioned cache, embedded data, then the embedded generator against that exact process. |

On Linux, vosu can also read the osu! path from `${XDG_DATA_HOME:-$HOME/.local/share}/osuconfig/osupath`.

Official Linux archives statically include libbpf and dynamically use the system `libelf.so.1` and `libz.so.1` libraries.

On Linux, `process_vm_readv` must be permitted. During ordinary startup vosu checks the running process's effective capability set and warns if `CAP_SYS_PTRACE` is not effective, or if that check cannot be completed. The warning does not stop startup, but memory reads may fail. Configure the exact executable with:

```sh
vosu --setup
```

An unprivileged `--setup` validates the current executable, then launches that exact executable and `--setup` through a trusted, root-owned, non-writable `pkexec`. Polkit owns the graphical or terminal credential prompt; vosu never reads, logs, or retains the password. An already-root setup skips `pkexec` and does not prompt. Authentication cancellation or failure, an unavailable/noninteractive authentication agent, and a missing trusted `pkexec` all exit unsuccessfully before `setcap` runs, so they leave the executable unchanged. There is no sudo fallback.

`pkexec` runs whatever file is at the executable's path, so a user-writable binary can be swapped before authentication. When the executable or its directory is not root-owned and write-protected, as in a portable ZIP under your home directory, unprivileged setup prints one warning line before the prompt and then continues. Distribution packages install a root-owned `/usr/bin/vosu` and never show it.

After authentication, the elevated setup directly invokes a trusted, root-owned `setcap` executable and exits without starting the daemon. A normal build receives only `CAP_SYS_PTRACE` and reports polling as its available discovery backend. A build compiled with `linux-process-events` first attempts its maximum compiled capability set. When that succeeds, it re-executes itself to probe eBPF and CN_PROC independently, then retains `CAP_SYS_RESOURCE`, `CAP_PERFMON`, and `CAP_BPF` only when eBPF actually works. If the maximum set is unavailable, setup retries with `CAP_SYS_PTRACE`, marks eBPF unavailable, and still probes CN_PROC before reporting polling as the final fallback. CN_PROC requires no additional file capability. Probe failures and the final capability set are printed. A failed capability transaction removes transient capabilities; if rollback itself fails, setup reports that the executable must be verified before use.

If `setcap` lacks authority, setup exits unsuccessfully without starting the daemon. Executable replacement, including an automatic update, removes file capabilities, so rerun `vosu --setup` after replacing vosu.

With `linux-process-events` compiled, an unset `VOSU_PROCESS_EVENTS` selects `auto`: runtime tries eBPF, falls back to CN_PROC, then polling. Set `VOSU_PROCESS_EVENTS=off` or `poll` to force polling, or select `ebpf` or `cn-proc` explicitly.

Do not run the ordinary vosu daemon as root to bypass ptrace or eBPF policy. Pattern or offset resolution may remain pending and retry while the process is alive; that alone does not confirm support for a game build.

## Files and migration

### Linux

The daemon follows XDG directories and appends `vosu`:

| Data | Path |
| --- | --- |
| Configuration | `${XDG_CONFIG_HOME:-$HOME/.config}/vosu/vosu.toml` |
| Counters | `${XDG_DATA_HOME:-$HOME/.local/share}/vosu/static/` |
| Counter schema | `${XDG_DATA_HOME:-$HOME/.local/share}/vosu/static/<counter>/settings.schema.toml` |
| Counter values | `${XDG_CONFIG_HOME:-$HOME/.config}/vosu/settings/<counter>.settings.values.toml` |
| Cache | `${XDG_CACHE_HOME:-$HOME/.cache}/vosu/` |

### Windows

The directory containing `vosu.exe` is the program, configuration, and data root:

| Data | Path |
| --- | --- |
| Configuration | `<program>/vosu.toml` |
| Counters | `<program>/static/` |
| Counter schema | `<program>/static/<counter>/settings.schema.toml` |
| Counter values | `<program>/settings/<counter>.settings.values.toml` |
| Cache | `<program>/.cache/` |
| Logs | Runtime log directory below the program location |

An absolute `STATIC_FOLDER_PATH` is used as written. A custom relative path is resolved from the launch directory. The defaults `./static` and `.\static` map to the platform data root.

On first start after the rename, vosu performs a one-time, idempotent state migration. On Linux it moves files from the former `tosu` XDG config, data, and cache roots into the corresponding `vosu` roots without overwriting state already present there, then removes empty former roots. On every platform, configuration migration layers defaults, readable `tosu.env`, readable `vosu.env`, and canonical `vosu.toml` with precedence `vosu.toml > vosu.env > tosu.env > defaults`. The legacy dotenv files are migration inputs only. It writes canonical TOML to `vosu.toml` through an atomic same-directory replacement; if canonical TOML is malformed at startup, all source files remain untouched. After migration, vosu reads and writes only canonical `vosu.toml`; legacy counter settings inputs remain untouched.
For each counter, the package root's `settings.schema.toml` is the authoritative schema and developer schema saves write that file. Steady-state config storage contains only mutable values at `<config>/settings/<counter>.settings.values.toml`. When the root schema is missing, vosu creates it from root `settings.json` when present, otherwise by inferring setting types from scalar root `settings.toml`: strings become `text`, integers/floats become `number`, booleans become `checkbox`, and each key supplies `uniqueID`/`title` with the scalar as its default. Arrays, nested tables, and datetimes are rejected as ambiguous without creating a schema or modifying package inputs. Only after package sources does loading fall back to old config `<counter>.settings.toml` or `<counter>.settings.json`; every legacy counter schema and values input remains byte-for-byte untouched after canonical schema and values are created. Root JSON and TOML inputs remain untouched, and when both exist JSON defines the schema while TOML may seed values.

## Configuration

Edit `vosu.toml` in the platform configuration root. It is a flat, typed TOML document with uppercase binding names. The daemon watches its parent directory and applies supported changes without restarting. Writes use a temporary file in the same directory, flush and sync it, then atomically replace the target.

```toml
ENABLE_AUTOUPDATE = true
OPEN_DASHBOARD_ON_STARTUP = true
START_AT_LOGIN = false
POLL_RATE = 150.0
PRECISE_DATA_POLL_RATE = 10.0
SERVER_IP = "127.0.0.1"
SERVER_PORT = 24050.0
DASHBOARD_IP = "127.0.0.1"
DASHBOARD_PORT = 24051.0
STATIC_FOLDER_PATH = "./static"
ALLOWED_IPS = "127.0.0.1,localhost,absolute"
```

At startup, malformed canonical TOML leaves every source file untouched. During watch reload, malformed TOML leaves the last-good runtime configuration and dependent resources active.

| Binding | Default | Meaning |
| --- | ---: | --- |
| `ENABLE_AUTOUPDATE` | `true` | Install stable vosu releases at startup. Self-updating release builds check for new releases at every start either way. |
| `OPEN_DASHBOARD_ON_STARTUP` | `true` | Open the effective dashboard listener after startup. |
| `START_AT_LOGIN` | `false` | Linux only: run vosu as a systemd user service at graphical login. |
| `DEBUG_LOG` | `false` | Include debug-level file and console logging. |
| `CALCULATE_PP` | `true` | Enable live performance calculations. |
| `ENABLE_KEY_OVERLAY` | `true` | Publish key-overlay data in compatible schemas. |
| `ENABLE_MANIA_KEY_HIT_ERRORS` | `false` | Opt in to per-column mania press/release timing arrays in vosu v2 and precise payloads. |
| `ENABLE_HIT_OFFSETS` | `false` | Opt in to osu!standard cursor-offset and timing triples in vosu v2 and precise payloads. |
| `POLL_RATE` | `150` | Regular state interval in milliseconds; range `100`–`60000`. |
| `PRECISE_DATA_POLL_RATE` | `10` | Precise state interval in milliseconds; range `1`–`60000`. |
| `V2_SUBSCRIPTION_DEBOUNCE` | `1` | Fast selected-field reader interval and minimum interval between subscription frames, in milliseconds; range `1`–`60000`, fractional values supported. Slow selectors do not accelerate readers. |
| `SHOW_MP_COMMANDS` | `false` | Include multiplayer commands where supported. |
| `READ_MANIA_SCROLL_SPEED` | `true` | Read mania scroll speed. |
| `SERVER_IP` | `127.0.0.1` | Overlay/data listener hostname or address (unchanged overlay origin). |
| `SERVER_PORT` | `24050` | Overlay/data listener port. |
| `DASHBOARD_IP` | `127.0.0.1` | Independent trusted dashboard listener, loopback-only by default even when overlays are shared on a LAN. |
| `DASHBOARD_PORT` | `24051` | Trusted dashboard and management port. |
| `STATIC_FOLDER_PATH` | `./static` | Counter root; the default maps to the data directory. |
| `ALLOWED_IPS` | `127.0.0.1,localhost,absolute` | Peer IPs/CIDRs, legacy IPv4 wildcard octets, and allowed Origin/Referer hostname tokens. Loopback peers are always allowed. `absolute` is a literal legacy hostname token, not a wildcard or authentication bypass. |

Disk configuration clamps oversized/nonfinite intervals to `60000` ms with a warning. Settings API updates above that ceiling fail without publishing or persisting any part of the update.

Network settings (`SERVER_IP`, `SERVER_PORT`, `DASHBOARD_IP`, `DASHBOARD_PORT`, `ALLOWED_IPS`) use **Apply network settings**, not autosave. Confirm **Keep network settings** from the new dashboard origin within 30 seconds; otherwise vosu restores the previous network configuration and listeners even if the browser closes. Only one change may be pending. Other settings still autosave. File edits remain live-rebindable without this dashboard-only confirmation workflow.

Use CIDR for networks: `100.64.0.0/10` is the Tailscale range; `100.*.*.*` is a much broader legacy wildcard covering public address space. IPv6 CIDRs such as `fd00::/8` are supported. Hostnames (including `localhost` and legacy `absolute`) permit matching browser provenance, never arbitrary non-loopback peers.

Authorization requires an allowed network peer and, when supplied, an allowed `Origin` or `Referer`. A hostname token does not allow an arbitrary remote peer. Keep the default loopback listener unless remote access is intentionally configured and protected.


## Dashboard and counters

vosu keeps a tray icon whenever the Linux desktop's StatusNotifier host or Windows notification area is available. **Open dashboard** opens the current effective listener (wildcard listeners map to loopback); **Quit** performs normal graceful shutdown. Headless sessions continue without a tray.

Launching vosu again checks the configured listener for vosu's identity, opens that instance's dashboard, and exits successfully. A different service on the same port is not treated as vosu and retains the normal bind-error behavior. Updater helper/candidate/rollback processes never perform this handoff or start a tray.

The dashboard at `/` discovers counters recursively below the static root. A counter normally contains `index.html`. `metadata.txt` can set `Name`, `Author`, `AuthorLinks` (comma-separated), `Version`, and `Resolution` (`WxH`); other keys are ignored. An immediate `ignore.txt` excludes its directory from discovery, and directories named `node_modules` are never traversed. Mutation APIs accept counter names containing one normal path component.

On Linux, **Settings → General → Start at login** manages a marked user unit at `${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/vosu.service`. Enabling records the current executable path and enables the unit for the next graphical session; it does not start another daemon now. Disabling removes only a unit written by vosu and does not stop the current daemon. An existing unrelated unit is never overwritten. Failed service changes leave the saved setting unchanged and report an error in the dashboard. The unit follows `graphical-session.target` so the tray has the desktop session's bus and display. This setting is hidden on Windows.

Discovery accepts at most 4096 named metadata/index files; each `metadata.txt` must be a regular file no larger than 64 KiB. Counter installation admits four concurrent jobs across static-root reloads; additional requests receive HTTP 429. Shutdown cancels and joins admitted jobs even when their initiating HTTP client disconnects.

A top-level directory symlink directly below the static root is an explicit local mount declaration and may target a directory outside that root; no `index.html` marker is required for authorization. Discovery follows that top-level alias but skips nested directory symlinks, which neither create nor traverse additional mount roots. Every requested file below the alias is still confined to the current alias target and streamed through a retained handle. Counter settings remain available through aliases, while final symlinked settings files are rejected and settings writes use atomic same-directory replacement. **Open Folder** follows a directory alias. **Delete** unlinks the alias itself without traversing or deleting its target.

Installed overlays appear as equal-size cards, at most four per row (five on screens at least 2240 CSS pixels wide): a preview frame that contains each counter at its declared `WxH` resolution (small counters are enlarged up to 2×), the name, version, author and size, and a URL bar that copies the counter's OBS URL on click (Shift-click opens it). Click a preview, or use **Enlarge preview** in the card's **⋯** menu, to open a live view scaled to fill a dialog. Pin a card with its pin toggle; pinned cards stay above unpinned cards in their saved order, and **Move pin earlier/later** in the **⋯** menu reorders them. That menu also holds **Open in browser**, **Copy resolution**, **Open folder**, **Nickname and URL…** and **Delete**. Pins are stored per browser origin, so `localhost` and `127.0.0.1`, or a different port, keep separate pins. Arrow keys move between cards; Tab moves through a card's pin, menu and URL. An overlay folder whose `metadata.txt` or settings cannot be read appears as an error card naming the problem, with **Open folder** and **Delete**; the other overlays still load. The Overlays page renders from local files only, and update badges come from the last catalog fetch. Available overlays are one listing: catalog-source overlays (official first, each with a source badge) above the community catalog, which shows one preview image at a time, with arrows to browse the rest and a click to enlarge, beside the name, author links, size, intended use, compatible clients, and the install or update action. Rows sit side by side when the window has room for more than one, and the preview stacks above the details on phones. Preview heights follow the window height. Preview images load from the catalog's image URLs (currently `raw.githubusercontent.com`) without a referrer. The page tabs and search live in the top bar; press `/` outside an editable control to focus search. Every search matches all of its words. Installed search filters the loaded cards in place, Available search queries the catalog and filters the source overlays, and Settings search filters the options. Runtime settings save individually when a field loses focus or a switch changes, with a "Saved" mark or the error beside the field; blank or out-of-range numbers are rejected in place rather than saved, and an absolute overlays folder that does not exist is rejected. Changing the listener address or port needs **Apply**; the dashboard then moves to the new address and pins a bar under the header with **Keep** (focused), **Revert now** and the seconds left. Without Keep the change reverts after 30 seconds and the page says so. The update notice can be put off with **Later**; the version chip in the status strip keeps the update one click away.

**Nickname and URL…** in a card's **⋯** menu sets a dashboard nickname (shown instead of the overlay's name on its card, in search, its enlarged preview, and its settings dialog) and a URL alias. A URL alias such as `acc` makes `http://127.0.0.1:24050/acc/` answer with a `302` redirect to the overlay's folder (`/accuracy%20by%20sxlvia/`), keeping any further path and query, so a browser source can use the short URL while the overlay still loads its relative files, settings, and profiles from its real folder. The card's URL bar then copies the alias URL. URL aliases are 1–48 lowercase letters, digits, `-` or `_`; they cannot reuse a top-level path vosu routes (`api`, `assets`, `files`, `json`, `tools`, `websocket`, `ws`, `settings`, …), another overlay folder, or another URL alias, and a folder created later under the same name takes precedence. Names are stored in `overlay-names.json` in the configuration directory, written atomically, kept when an overlay is updated in place, and dropped when it is deleted or its folder disappears (for example after a rename outside vosu).

Counter settings support text, color, number, checkbox, button, options, commands, textarea, password, header entries. Each counter package owns its authoritative schema at `<static-root>/<counter>/settings.schema.toml`; only mutable values are stored centrally at `<config>/settings/<counter>.settings.values.toml`. `/settings` page handles daemon counter settings.

The **Available** page lists community overlays from the upstream tosu catalog at `https://tosu.app/api.json`; vosu does not operate a website or catalog service. Listed overlays are made and maintained by their respective authors, not by vosu, and vosu does not review or endorse them. vosu is not affiliated with the tosu project and does not operate or control tosu.app or osuck.link, which currently hosts the overlay downloads; report catalog or download outages to the [tosu project](https://github.com/tosuapp/tosu/issues). The page states this above the listing and links to the [upstream catalog repository](https://github.com/tosuapp/counters) and the [privacy policy](PRIVACY.md). The catalog has no single license: each author keeps the rights to their overlay, and vosu does not bundle, mirror, or redistribute overlays. **Install** downloads the archive from the link the catalog lists for it; check that overlay's own license before you modify or redistribute it.

A failed install names the download host when the host caused it: an error status, timeout, connection or redirect failure, or a response that is not a ZIP archive (some hosts answer rate limits with an HTTP 200 text page).

Production counter downloads require HTTPS. Redirects are bounded; HTTPS-to-HTTP downgrades and loopback, private, or reserved destinations are rejected. ZIP paths, symlinks, entry counts, and expanded size are checked before same-filesystem replacement.

## HTTP API

Methods and paths are exact. Unsupported combinations return `404 Not Found`. State endpoints return `500 {"error":"osu is not ready/running"}` until an instance is focused.

### State and calculation

| Route | Response |
| --- | --- |
| `GET /json` | gosuMemory-compatible v1 snapshot |
| `GET /json/sc` | streamCompanion-compatible snapshot |
| `GET /json/v2` | vosu v2 snapshot |
| `GET /json/v2/precise` | vosu precise snapshot |
| `GET /api/calculate/pp` | Legacy `rosu-pp` calculation; uses the current beatmap unless `path` names a canonical regular `.osu` file |

Calculator file inputs are limited to 64 MiB, including opened-handle checks and bounded reads. The live calculator facade additionally admits at most 250,000 parsed objects with finite start timestamps within ±10,000,000 ms. Supported effective clock rates are `0.01`–`100`. Graph output is limited to 1,000,000 aggregate bins; excessive audio padding yields an empty graph. These are per-input limits, not a process-wide concurrent-calculation memory ceiling.

The v2 snapshot and matching WebSocket streams expose `beatmap.stats.hitWindow`. The object has optional numeric judgement fields `miss`, `meh`, `ok`, `good`, `great`, and `perfect`; unsupported judgements are omitted. Values are maximum absolute timing errors in milliseconds after the active clock rate is applied. An unparsed/missing map serializes an empty object. Catch publishes all six fields as zero, matching upstream's empty-window implementation, while `stars.hitWindow` remains absent. Catch's spatial catcher width is not a timing window.

The tosu hit-error arrays keep their original names and shapes: `hitErrorArray` in v1 and regular v2, and `hitErrors` in streamCompanion and precise v2. Vosu adds sibling `pressHitErrors` and `releaseHitErrors` arrays. The original array preserves every timing event in order, `pressHitErrors` contains note presses, and `releaseHitErrors` contains osu!mania hold-note releases; together they hold every value of the original array exactly once, in order. osu!lazer classifies from its judgement results. osu!stable classifies native mania maps, including with the Mirror mod, by matching each error to the map's judgement times and the replay's key transitions; classification can lag the original array while that evidence arrives. An error that matches no transition (for example under the Random mod, whose column shuffle vosu does not reproduce) is published as a press once the replay has moved 250 ms past it or one second has passed, so it never holds back later errors. vosu keeps the newest 8192 replay frames, so when it attaches to a play that already has more, hits whose key transitions have been dropped are published as presses too; later hits are classified normally. Stable converted maps and maps whose metadata is unavailable stay press-only, because vosu does not reproduce stable's osu!-to-mania lane conversion. osu!, taiko and catch have no release judgements.

Hit-error capture and appends require a fresh `play` state, including the opening 150 ms. Polling before the first object, a temporarily shrinking native event list, and unavailable precise status do not clear accumulated arrays. Pause and results retain the current attempt's history. Returning to song selection or otherwise exiting the attempt clears it; a genuine quick retry starts a fresh attempt.

With `ENABLE_MANIA_KEY_HIT_ERRORS = true`, v2 `play` and the precise payload additionally expose `keyHitErrors: { "press": number[][], "release": number[][] }`. The outer index is the zero-based displayed mania lane, including Mirror; untouched lanes before the highest observed lane are empty. The field is omitted entirely by default and is never added to v1 or streamCompanion. Outside mania attempts both outer arrays are empty. History follows the existing hit-error reset/retention rules; enabling mid-attempt starts per-key history with subsequent classifications, without rebuilding earlier events.

Stable includes only errors matched to a native mania map judgement and replay transition. Unmatched errors published by the 250 ms/one-second fallback, Random-mod plays, and converted maps have unknown columns and are excluded from these per-key arrays (but remain in the original aggregate arrays). Lazer reads each judged object's column and moves a tail's timing from that column's press array to its release array if its hold aggregate arrives later. Missing/unmeasured column offsets omit those events rather than inventing a lane.

Regular and precise v2 responses include a mode-free `keys` object. Its tosu-compatible `k1`, `k2`, `m1`, and `m2` members and stable in-game-overlay source remain unchanged. The additive `all` array contains ordered logical actions with `name`, `isPressed`, `count`, `pressedAt`, and `releasedAt`. Names are `B1` onwards on both clients, not physical key names. `ENABLE_KEY_OVERLAY` still gates key updates after the opening 150 ms. Stable `all` follows replay frames even when its in-game overlay is hidden; legacy members do not switch to frames.

| Ruleset | `keys.all` order (stable and lazer) | Stable replay source |
| --- | --- | --- |
| osu! | `B1` left button, `B2` right button, `B3` Smoke | bits `1 or 4`, `2 or 8`, `16`; keyboard/mouse inputs merge into logical actions |
| taiko | `B1` left rim, `B2` left centre, `B3` right centre, `B4` right rim | bits `2`, `1`, `4`, `8` respectively |
| catch | `B1` move left, `B2` move right, `B3` dash | left/right are **never pressed**, counts zero and timestamps null: stable records position, not these actions; dash is button state exactly `1` |
| mania | `B1` through `BN`, displayed column order | x-slot column bitfield, bit 0 first; N is the converted map key count (including key mods/dual stages) |

The mapping is source-verified against osu!lazer [2026.921.0-lazer](https://github.com/ppy/osu/tree/2026.921.0-lazer): [trigger ordering](https://github.com/ppy/osu/blob/2026.921.0-lazer/osu.Game/Rulesets/UI/RulesetInputManager.cs), [trigger names](https://github.com/ppy/osu/blob/2026.921.0-lazer/osu.Game/Screens/Play/HUD/KeyCounterActionTrigger.cs), default bindings in [osu](https://github.com/ppy/osu/blob/2026.921.0-lazer/osu.Game.Rulesets.Osu/OsuRuleset.cs), [taiko](https://github.com/ppy/osu/blob/2026.921.0-lazer/osu.Game.Rulesets.Taiko/TaikoRuleset.cs), [catch](https://github.com/ppy/osu/blob/2026.921.0-lazer/osu.Game.Rulesets.Catch/CatchRuleset.cs), and mania [single](https://github.com/ppy/osu/blob/2026.921.0-lazer/osu.Game.Rulesets.Mania/SingleStageVariantGenerator.cs)/[dual](https://github.com/ppy/osu/blob/2026.921.0-lazer/osu.Game.Rulesets.Mania/DualStageVariantGenerator.cs) stage generators. Triggers enumerate distinct **default bindings**, not available-action enums. Mania single-stage 4K–10K yields B1–B4 through B1–B10; dual-stage N+N yields B1–B(2N), stage one then stage two, up to 20. The lazer reader previously capped entries at 18; it now supports 20, without changing existing names/order.

Stable encodings follow upstream legacy replay conversion: [osu](https://github.com/ppy/osu/blob/2026.921.0-lazer/osu.Game.Rulesets.Osu/Replays/OsuReplayFrame.cs), [taiko](https://github.com/ppy/osu/blob/2026.921.0-lazer/osu.Game.Rulesets.Taiko/Replays/TaikoReplayFrame.cs), [catch](https://github.com/ppy/osu/blob/2026.921.0-lazer/osu.Game.Rulesets.Catch/Replays/CatchReplayFrame.cs), [mania](https://github.com/ppy/osu/blob/2026.921.0-lazer/osu.Game.Rulesets.Mania/Replays/ManiaReplayFrame.cs), and [button flags](https://github.com/ppy/osu/blob/2026.921.0-lazer/osu.Game/Replays/Legacy/ReplayButtonState.cs). Live stable (`b20260924.1`) confirmed the in-memory button byte for osu! keys and smoke, all four taiko drums and catch dash, and that `keys.all` keeps counting with the in-game key overlay hidden while the legacy members stay at zero.

Edge timestamps are nullable gameplay milliseconds. Stable counts every retained frame down-edge, with exact frame timestamps, including complete taps between polls; state stops at the current play clock, even for preloaded replays/spectating. Retry/rewind resets the attempt. Rolling-window eviction preserves accumulated counts; if the consumer misses more than 8192 frames, it takes the first available state as a baseline without inventing a press, so lost edges cannot be recovered. Attaching midway likewise cannot recover evicted history. Lazer retains sampled-edge timestamps: a count jump can report missed complete taps without fabricating timestamps. Catch left/right and unrecoverable history are remaining parity differences, not physical-input measurements.

Catch gameplay also publishes the vosu-only `catcher: { "x": number, "dashing": boolean }` object in v2 `play` and at the precise payload's top level. It is omitted in menus, results, and other rulesets. `x` is the client's stored playfield position in osu! pixels (0–512), not a screen-space coordinate; `dashing` is ordinary dash, not hyperdash. Stable selects the replay frame at the current play time (including preloaded watched replays), independently of new judgements. The legacy x slot and dash value 1 follow [lazer's legacy catch replay conversion](https://github.com/ppy/osu/blob/2026.921.0-lazer/osu.Game.Rulesets.Catch/Replays/CatchReplayFrame.cs); live stable autoplay confirmed both. Lazer reads the catcher's native X and Dashing fields during precise polling, using the measured catch entries of the offsets asset or runtime generation (see [offset maintenance](docs/operations/offsets-asset.md)).

With `ENABLE_HIT_OFFSETS = true`, v2 payloads additionally expose `play.hitOffsets`
(regular v2) and top-level `hitOffsets` (precise v2): a flat stride-3 array `[x, y, error, …]` for osu!standard
circle and slider-head hits. `x`/`y` are cursor minus stacked center in displayed
512×384 playfield pixels; `error` is the original matching timing error in ms.
The array is empty outside standard gameplay; missing spatial evidence omits a
triple rather than inventing a position. Stable matches replay down edges to
unjudged map heads using `press time - error` (±1 ms), with calculator stacking
and HR reflection: HR reflects the raw position, and the stack offset is then
applied unreflected, as stable's own HR autoplay cursor shows. Pending evidence
expires after 250 ms and never blocks later
matched judgements. Mirror and other positional transformations are unsupported
on stable (Mirror produces no triples). Lazer reads the actual judgement cursor
and object's position/stack/scale, so its applied positional mods need no map
reconstruction. Lazer needs the measured spatial offsets from the embedded asset
or runtime generation; without them it emits no triples. These fields are not
added to v1 or streamCompanion.
The field is omitted entirely by default. While disabled, stable skips spatial
replay/map matching and lazer skips judgement-position and object-property reads.
Enabling mid-attempt records subsequent hits only; it does not backfill history.
`keys.all` and `catcher` are independent of this setting.

These additive hit-error and `keys.all` fields are planned for a tosu port soon(tm). Until that lands, overlays targeting both servers must treat them as optional. Existing tosu overlays can use vosu's regular and precise v2 streams without adapting to the extensions.

Maintainers can run the zero-dependency harness with `cd tools/input-validation && uv run python run.py ...`. `capture` accounts for precise transitions and `scan-maps` selects long-note-heavy mania maps. `autoplay <map.osu>` and `live-autoplay <map.osu>` generate deterministic plans for osu!, taiko, catch, and mania, then inject them through Windows `SendInput` or Linux `/dev/uinput`; live mania plans additionally support bounded seeded timing jitter. Standard-mode and catch pointer injection requires `--playfield x,y,width,height`; Linux also requires the full compositor layout as `--desktop x,y,width,height`, not only the target monitor. Default controls are `Z` for osu!, `F,J,D,K` for taiko plan controls `DON_LEFT,DON_RIGHT,KAT_LEFT,KAT_RIGHT`, pointer-only for catch, and `D,F,J,K` for 4-key mania.

Maintainer workflow, evidence interpretation, troubleshooting, and maintenance triggers are documented in the [precise input validation guide](docs/testing/input-validation.md).

`navigate` follows keyboard paths from an unobstructed idle logo: lazer uses `P,P,P` for song select and `P,P,M,M` for multiplayer, while stable uses `P,P,P` and `P,P,M`. Navigation key-down onsets default to 100 ms apart. Optional song-select search text uses 1 ms key-down onset spacing. Use each command's `--help` for backend, timing, geometry, and key-map options.

All calculation parameters are optional: `path`, `lazer`, `mode` (`0` osu, `1` taiko, `2` catch, `3` mania), `ar`, `cs`, `hp`, `od`, `clockRate`, `passedObjects`, `combo`, `nMisses`, `n100`, `n300`, `n50`, `nGeki`, `nKatu`, `mods` (legacy bits or a JSON mod object/array), `acc`, `sliderEndHits`, `smallTickHits`, `largeTickHits`, and `hitresultPriority`.

### Renderer state (v2)

Tools following the current map can subscribe to these additive fields through
`/websocket/v2/subscribe` (the same fields appear in `/json/v2`):

| JSON Pointer | Meaning |
| --- | --- |
| `/play/source` | `null` outside gameplay; `player` for local play (including multiplayer), `replay`, `spectator`, `autoplay` (AT/CN), or `watching` for stable's combined replay/spectate flag. A preview must hide for `state.number == 2` and `source == "player"`; treat missing/unknown sources during play as hidden too. |
| `/beatmap/time/live` | Existing absolute map clock in milliseconds, including the song-select preview track. Do not replace it with time since opening the tool. |
| `/beatmap/time/rate`, `/beatmap/time/running` | Actual lazer track frequency × tempo and decoupled-clock running flag. They follow custom rates and dynamic rate-adjust mods, rather than guessing from acronyms. Null when unavailable. Stable rate uses its fixed selected/gameplay mods; stable running remains null because no authoritative flag is read. |
| `/settings/mode/number` | Current effective ruleset, including converts. `/play/mods/array` contains current song-select mod selection (with lazer settings), switching to gameplay/results mods in those states. Unset lazer random seeds are null until the client chooses one. |
| `/settings/mania/scrollSpeedValue`, `/settings/mania/scrollTime` | Optional lazer double-precision speed and unadjusted base visible-time range `11485 / speed` ms. Existing integer `scrollSpeed` is retained. The renderer must still apply rate and skin hit-position scaling; do not apply this lazer formula to stable. |
| `/settings/mania/scrollDirection` | Existing `{number,name}` (`0/up`, `1/down`). Lazer reads its ruleset setting. Stable has no equivalent read; its skin.ini `UpsideDown` must be honoured by the renderer. |
| `/settings/mania/timingBasedNoteColouring` | Optional lazer ruleset boolean. |
| `/settings/mania/perBeatmapScrollSpeed` | Optional stable current-map speed when per-map speeds are enabled and its memory value is available (`READ_MANIA_SCROLL_SPEED` must be enabled). |
| `/settings/visual` | Optional object with nullable `snakingInSliders`, `snakingOutSliders`, `hitLighting`, `beatmapColours`, `hitAnimations`, `rateAdjustedHitAnimation`. Lazer reads client/ruleset config; stable exposes its `SnakingSliders` and `HitLighting`. Ruleset-dependent animation toggles describe the selected ruleset, not a previously selected one. Beatmap skin permission remains the inverse of `/settings/skin/ignoreBeatmapSkins`. |
| `/settings/visual/comboColourNormalisation` | Lazer's current `ComboColourNormalisationAmount` float (`0`–`1`, client default `0.2`), read using the client's generated configuration key. Stable reports null; an unavailable lazer value is null, never replaced with the default. |
| `/settings/visual/scalingMode`, `/settings/visual/positionX`, `/settings/visual/positionY`, `/settings/visual/sizeX`, `/settings/visual/sizeY` | Optional lazer viewport scaling mode and normalized position/size settings; unknown values are null. Skin hit positions and column widths remain skin resources, not separate client configuration. |

Selected-field sampling follows the requested fields' producing reader, rather
than merely the presence of a subscriber:

| Selection | Additional reader demand |
| --- | --- |
| Client, folders, settings values (including `/settings/mode`), beatmap checksum/metadata and file paths | None; configured polling continues unchanged. |
| `/keys`, hit-error arrays, key errors, hit offsets and catcher state | Precise reader, only during play (including replay/spectate). |
| Score, accuracy, HP, hits, combo, mods, rank, pp, leaderboard and live gameplay map statistics | Regular reader, only during play. Unstable rate needs both lanes. |
| `/state`, `/beatmap/time/live` | Precise reader, also outside play for status transitions and the song-select audio clock. |
| `/session/playTime`, `/settings/bassDensity`, `/room` | Regular reader, also outside play. |

Parent pointers inherit their descendants' demand: `/play` selects both gameplay
lanes; `/settings` includes bass density; `/beatmap` includes the live clock; the
empty pointer selects the whole document and both lanes. Select narrow leaves
when following identity or settings only. Multiple clients' demands are unioned
and released when those clients disconnect.

Only a demanded, currently active lane uses the smaller of its configured
interval and `V2_SUBSCRIPTION_DEBOUNCE` (default 1 ms). Other lanes retain
`POLL_RATE` / `PRECISE_DATA_POLL_RATE`; gameplay-only demand goes dormant outside
play. Debounce still limits frames for every subscription, including slow ones.
This permits fast updates but is not a guaranteed frame rate. Publication and
backpressure stay latest-only. Missing but syntactically valid JSON Pointers are permitted and
report per-pointer absence; they do not invalidate the whole subscription.

Beatmap selection changes bypass the compatibility API's regular polling interval:
the checksum and its current beatmap/audio/background paths are published together
before selected-field subscribers are notified. Between selection changes, regular
fields retain their configured API cadence. Separate requests are not a transaction: if the user changes
selection again during a fetch, consumers should compare the downloaded map's MD5
with their latest selected checksum.

### Current game files

- `GET /backgroundImage`
- `GET /files/beatmap/background`
- `GET /files/beatmap/audio`
- `GET /files/beatmap/file`
- `GET /Songs/{path}`
- `GET /files/beatmap/{path}`
- `GET /files/skin/`; current skin metadata `{"client":"stable"|"lazer"|null,"name":string|null,"files":bool,"builtin":string|null,"base":string|null,"entries":[{"path":string,"size":number,"modified"?:number,"hash"?:string}],"truncated":bool}` without absolute paths. The existing `files` boolean is unchanged. Stable entries carry `modified` (mtime in milliseconds since Unix epoch); lazer entries carry the content-store `hash` instead. `base` identifies the rendering family (`argon`, `argon-pro`, `triangles`, `legacy`, `retro`) for built-in **and imported** skins; unknown families are null. Stable skins use `legacy`.
- `GET /files/skin/{path}`; stable and imported lazer skin files, with case-insensitive relative filename/subfolder lookup on both clients. Exact stable paths win; otherwise the complete index selects the lexicographically smallest matching path. Missing/unavailable lazer files return 404. Built-in lazer skins have no files: `files:false`, `entries:[]`, and `builtin` equal to `argon`, `argon-pro`, `triangles`, `legacy`, or `retro`.

File routes enforce root containment and support validated single HTTP byte ranges for media.

The skin index contains at most 20,000 files and visits at most 40,000 directory
entries. `truncated:true` means it is incomplete (including inaccessible entries
or directory aliases/cycles); consumers must retain probing as a fallback. Paths
use `/` separators and are relative to `/files/skin/`. Indexes are cached by skin
identity/root; metadata requests validate cached file and directory timestamps,
so edits/additions/removals are reflected without switching skins. Exact file
reads remain available beyond the listing cap. Unindexed/new files are resolved
component-by-component, preferring exact component names then lexical case-folded
matches. Every final open still enforces the existing root/symlink confinement.

`/files/skin/`, skin files and `/files/beatmap/*` files support `HEAD`, strong
`ETag`, and `Cache-Control: no-cache` (store and revalidate, not `no-store`).
File `Last-Modified` uses the opened file's mtime where representable as an HTTP
date. Lazer ETags use the store hash; stable ETags hash path, size and high-resolution
mtime without reading file contents. `If-None-Match` (including weak comparisons,
lists and `*`) takes precedence over `If-Modified-Since`; a matching condition
returns bodyless 304. HEAD returns the normal full-file headers with no body and
ignores Range; GET retains existing single-range behaviour. Skin metadata has a
content-derived ETag and may also be revalidated. Peer policy/CORS are unchanged.

Lazer skins use content-addressed storage, not an export folder. Consequently v2
`folders.skin` / `directPath.skinFolder` retain their existing compatibility
semantics rather than exposing the files store as a fictional skin folder.
Tools should use `/files/skin/` metadata and `/files/skin/{path}` instead; the
permission-filtered tool context exposes the same availability and built-in identity
while keeping `skin.folder:null` for lazer.

### Pages, counters, reports, and updates

- `GET /`, `/settings`, `/available`, `/favicon.ico` (favicon responses add `X-Vosu-Identity: vosu` for bounded second-launch discovery without changing page bodies)
- `GET /assets/{path}` and the fallback counter-static route
- `POST /api/settingsSave` rejects the whole save when a submitted interval or port is not a finite number or is out of range: `POLL_RATE` 100–60000, `PRECISE_DATA_POLL_RATE` 1–60000, `V2_SUBSCRIPTION_DEBOUNCE` 1–60000 (milliseconds), and both `SERVER_PORT` and `DASHBOARD_PORT` whole numbers 1–65535. Network fields require `_network_action: "apply"` and then `_network_action: "confirm"` from the new origin before the 30-second server timeout.
- `POST /api/runUpdates` returns an HTTP 200 JSON `error` for externally managed installs instead of attempting replacement.
- `POST /api/generateReport`
- `GET /api/counters/search/{query}[?source=installed|available]` returns card HTML for installed counters by default or for the catalog with `source=available`; any other `source` returns 400. The query keeps letters, digits, and whitespace, and a card matches when any word matches its name, author, or compatible clients. Lookup failures return 502 with an escaped `Error:` body.
- `POST /api/counters/download/{url}?name=<counter>&update=<value>`
- `POST /api/counters/open/{name}`
- `POST /api/counters/open-root` opens the configured counter/static root.
- `POST /api/counters/delete/{name}`
- `GET|POST /api/counters/settings/{name}`
- `GET /api/counters/profiles` returns `{"active":"default","profiles":["default"]}` initially.
- `POST /api/counters/profiles` accepts one JSON action: `{"action":"create","name":"stream"}` (optionally `"copyFrom":"default"`), `{"action":"rename","name":"stream","target":"live"}`, `{"action":"delete","name":"live"}`, `{"action":"activate","name":"stream"}`, or `{"action":"copyOverlay","name":"counter-folder","source":"default","target":"stream"}`. Success returns the updated registry; errors follow the counter API's HTTP 200 `{"error":"..."}` convention. POST requires dashboard provenance and CSRF, a `profiles:manage` API key, or the loopback native broker token. Unknown actions fail.

### Overlay settings profiles

Profiles group per-overlay values; schemas remain shared. The reserved `default` profile
uses the existing `<config>/settings/<counter>.settings.values.toml` files without moving
them. Others use `<config>/settings/profiles/<profile>/<counter>.settings.values.toml`;
`<config>/settings/profiles.json` records the default profile (API field `active`) and names. Files are
atomically replaced in their own directory. Names are case-sensitive references but
case-insensitively unique, contain only ASCII letters, digits, `_` and `-`, and are
1–48 characters. At most 32 profiles including `default` are allowed. `default` cannot
be renamed or deleted; deleting another profile currently used as the default selects
`default`. Empty profiles use schema defaults for overlays without saved values,
never another profile's values.

`GET /api/counters/settings/{name}?profile=stream` returns the unchanged
`{"settings":[...],"values":{...}}` shape. POST to the same URL accepts the existing
`[{"uniqueID":"field","value":...}]` body and returns `{"result":"success"}`.
Omitting `profile` selects the default profile. `update=yes` still edits the
shared developer schema. Unknown profile references return a JSON error.

The dashboard card editor and `/settings?overlay=<name>[&profile=<profile>]` inline
editor expose profile selection, creation/copying, rename, deletion and **Set as
default** (API action `activate`). The dropdown marks the default profile for
unpinned overlays with a `(default)` suffix; the reserved `default` profile needs no suffix.
Search matches titles, descriptions and IDs; matching sections retain their headers.
The sticky section navigation tracks the current section as the editor scrolls,
and is absent for schemas without headers. All dashboard modals lock background
page scrolling until closed, including Escape and backdrop dismissal.
Native editors such as vosulay use the HTTP settings and profiles API through
their Rust host; dashboard pages cannot be framed and have no embed mode or
parent-window notifications.

Loading this page through `http://localhost:<port>` works with the default
127.0.0.1 listener: mutations compare the page Origin to the request Host, not to
the listener's bind address. `localhost` is also in the default allowed host list.
Mixing `localhost` and `127.0.0.1` between Origin and Host is rejected.

An overlay URL may pin one instance with `?vosuProfile=stream`. Unknown names fall
back to the current default profile when serving the page. With no override the
served counter bytes and injected metadata are unchanged. With an override vosu
injects `window.COUNTER_PATH="/@vosu-profile/stream/<counter>"` and
`window.VOSU_PROFILE="stream"`. The leading slash and multi-component reserved
prefix cannot be a real counter-folder name. Unmodified tosu command clients use
this qualified value for their `l` socket parameter and `getSettings`, `saveSettings`
and `updateSettings` frames; the values resolve to the pinned profile. **Limitation:**
overlays that build asset URLs from `COUNTER_PATH` rather than their page URL must
adapt before using an override; the qualified identity is not a static-file URL.

Setting a default profile refreshes unpinned clients over `/websocket/commands`.
Saving or copying overlay values refreshes clients pinned to that profile and,
when it is the default, unpinned clients. Existing bounded command-relay queues and
save coalescing remain in force. Renaming/deleting a pinned profile requires
reloading the instance with a valid override; existing socket identities do not change.
See [ADR 0014](docs/decisions/0014-overlay-settings-profiles.md).

POST bodies are capped at 1 MiB. Generated reports escape machine, configuration, counter, and log data before inserting it into HTML.

Management POSTs require the existing trusted-peer policy, exact **dashboard-origin** browser provenance, and the per-start `X-Vosu-CSRF` token injected only into dashboard pages. `Origin` is authoritative; only when absent may a same-origin `Referer` authorize the action. Missing, opaque (`null`), malformed, or duplicate provenance is not mutation intent. Forwarded headers are not trusted. Rejected mutations return 403. The overlay listener rejects management mutations even from same-origin overlay scripts; scoped API-key settings/profile writes and overlay command-socket settings writes remain available.

The dashboard moved to `http://127.0.0.1:24051`; overlays remain on `http://127.0.0.1:24050/<overlay>/` (including the `localhost` alias), so their localStorage/IndexedDB origins do not change. Dashboard previews load that overlay origin, never overlay code on the dashboard origin. Overlay-origin GETs to `/`, `/settings`, and `/available` redirect with path/query preserved to the configured dashboard address, never an arbitrary request Host. `GET /api/dashboard` returns `{"origin":"http://127.0.0.1:24051"}` using the effective dashboard bind, with wildcard addresses mapped to loopback. Tray, startup, and second-launch links use the dashboard origin. See [ADR 0016](docs/decisions/0016-separate-dashboard-origin.md), superseding the shared-origin portion of ADR 0013.

Read-only JSON, static/file routes, counter-settings GETs, and state/settings WebSockets need no new headers or tokens. Headerless and opaque/file-origin reads remain available to trusted peers. Hosted stock counters retain `window.COUNTER_PATH` and the existing settings protocol unchanged. On legacy sockets, `getSettings`, `updateSettings` replies, and `applyFilters` remain readable without mutation authorization; `saveSettings` requires same-origin provenance captured at upgrade or the native command-broker credential described below, otherwise the socket closes with 1008 before saving or relaying. File-loaded and other non-hosted overlays may read settings but must use the hosted settings page or an authenticated native broker to write them.

### Native overlay brokers

An external native broker such as vosulay may save counter settings without
forging `Origin`. Each daemon start generates 32 OS-CSPRNG bytes, encoded as 64
lowercase hex characters, and atomically publishes
`{"version":1,"origin":"http://127.0.0.1:24050","token":"<64 hex>"}`.
The origin uses the listener's actual port and a connectable address, matching
dashboard discovery: wildcard `0.0.0.0` becomes `127.0.0.1`, and wildcard `::`
becomes `[::1]`. Specific binds remain unchanged, with IPv6 brackets when applicable.
A successful listener rebind updates it. The token still authenticates only loopback peers.

- Linux: `$XDG_RUNTIME_DIR/vosu/broker.json`; when `XDG_RUNTIME_DIR` is unset
  or empty, `${XDG_CONFIG_HOME:-$HOME/.config}/vosu/broker.json`. Both the
  runtime path and config fallback must be absolute; invalid paths disable
  broker support with a warning. Directory mode is 0700 and file mode is 0600,
  including during atomic replacement.
- Windows: `%LOCALAPPDATA%\vosu\broker.json`, never the program folder. The
  file inherits the directory ACL; a pre-existing directory with broader
  inheritable permissions also exposes the token. Protect that directory and
  its parents; vosu does not reset Windows ACLs.

Send `Authorization: Bearer <token>` to the file's loopback origin (`localhost`
and `127.0.0.1` may be treated as equivalent), for `/websocket/commands`
upgrades, `POST /api/counters/settings/<name>[?profile=<name>]` value saves,
`POST /api/counters/profiles` actions, and the
[`/tools/_host/v1/cli` launcher endpoints](docs/tools/protocol.md#command-line-tools-vosux).
Counter HTTP permissions are `settings:write` and `profiles:manage`: schema
writes (`?update=yes`) and other management routes remain forbidden (401).
Tool launcher routes require broker authentication even for listing.
vosu accepts the token only from loopback peers and compares it in constant time,
then revalidates after reading each POST body. Invalid, stale, wrong-scope or
non-loopback credentials return 401 even when browser provenance permits a write.
Restart rotates the token; reread the file rather than caching stale credentials.
Keep the token in the native host, never page JavaScript or logs.
The Bearer scheme is case-insensitive; other Authorization schemes (such as
Basic from a reverse proxy) are ignored, preserving their previous behavior.
If credential creation/publication fails, vosu logs a warning and continues
serving with broker writes disabled; a successful listener rebind still updates
the dashboard address. Replacement uses the same platform atomic-replace path
as other durable writes, including Windows sharing-violation retries.

On 401, reread the file and retry once; if that fails, connect without the token
for the existing read-only behavior. The file is never deleted: after vosu stops
its token is stale and fails with 401, and the next start atomically replaces it.
Deleting it on shutdown could race a newer daemon's file. Never log the token or expose
it to page JavaScript. A broker must authorize its inbound requests using its own
counter-frame Origin/Host and session secret before forwarding with this
credential: it is a privileged bridge, not an open local proxy.
Same-account processes able to read the file can write counter settings and
invoke other command-socket mutations. This is explicit native-client trust,
not isolation beyond the existing ability of native clients to forge Origin.


### API keys

Settings → **API keys** creates named credentials for trusted sandboxed, remote,
or web software. Copy a new key immediately: vosu shows it only once. You can
rename or revoke it later; permissions cannot be edited (revoke and recreate).
Revocation immediately closes associated command sockets (1008), even while idle.
Keys are managed only through the trusted dashboard.

- `settings:write`: existing command-socket writes and
  `POST /api/counters/settings/<name>` value saves, never `?update=yes` schema saves.
- `profiles:manage`: all `POST /api/counters/profiles` actions.
- Neither scope grants updates, install/delete, reports, daemon settings, or
  key management. Those require dashboard-origin provenance and its CSRF token.

Native WebSocket clients set this header on the `/websocket/commands` upgrade:

```http
Authorization: Bearer vosu_k_<64 lowercase hex characters>
```

For HTTP value saves (replace the example key with your saved secret):

```sh
curl -H "Authorization: Bearer $VOSU_API_KEY" -H 'Content-Type: application/json' \
  --data '[{"uniqueID":"title","value":"My overlay"}]' \
  http://127.0.0.1:24050/api/counters/settings/my-overlay
```

Browser WebSockets cannot set Authorization; offer both subprotocols instead:

```js
const socket = new WebSocket("ws://127.0.0.1:24050/websocket/commands?l=my-overlay",
  ["vosu.key", "vosu.key." + key]);
// socket.protocol === "vosu.key": vosu never echoes the secret subprotocol.
```

No URL query parameter authenticates a key. Cross-origin browser HTTP is not
supported: CORS does not allow Authorization. Invalid/revoked keys and insufficient
scopes return 401 (JSON for HTTP POSTs). Without a key, existing behavior remains.
Keys work remotely only for peers allowed by `ALLOWED_IPS`; they never bypass that
peer policy. API keys persist across restarts; the loopback broker token rotates
on every daemon start. Both authorize only their documented scoped HTTP writes.

The config root holds `api-keys.toml`: `${XDG_CONFIG_HOME:-$HOME/.config}/vosu/`
on Linux, the program directory on Windows. It contains SHA-256 hashes, not
recoverable secrets, and public metadata (name, scopes, short prefix, creation
and last-use timestamps). Unix files are created 0600 and atomically replaced;
Windows inherits directory ACLs. At most 64 keys exist. Last-use persistence is
throttled to once per minute per key. Malformed/unreadable files disable all keys
with a warning without stopping vosu or overwriting the file; an explicit key
management change replaces that registry.
On Unix, loading a group/world-accessible file or one owned by another user
warns without disabling otherwise valid keys. Restore private ownership and 0600 permissions.

Dashboard-only management: dashboard-origin provenance and `X-Vosu-CSRF` are required for `GET /api/keys`, which returns `{keys:[...]}`
without secrets/hashes; `POST /api/keys` accepts
`{action:"create",name,scopes}` (returns `{key,entry}` once),
`{action:"rename",id,name}`, or `{action:"revoke",id}`. Names are 1–64 printable
characters, excluding Unicode bidirectional controls; scopes contain one or both values above.

Treat keys as passwords. Plain HTTP/WS exposes them to network observers on a
LAN: keep loopback by default; use a trusted TLS reverse proxy for remote access.
Do not put keys in URLs, logs, public pages, or untrusted overlay code.
See [ADR 0015](docs/decisions/0015-user-managed-api-keys.md).

## WebSocket API

| Upgrade path | Stream |
| --- | --- |
| `/ws` | v1 regular state |
| `/tokens` | streamCompanion regular state |
| `/websocket/v2` | vosu v2 regular state |
| `/websocket/v2/precise` | vosu precise state |
| `/websocket/v2/subscribe` | Selected fields from the vosu v2 document |
| `/websocket/commands` | Counter settings and filter commands |

Unknown near-miss paths do not upgrade. Frames and assembled messages are limited to 1 MiB. Slow clients receive the latest state instead of building an unbounded backlog. The [upstream tosu wiki](https://github.com/tosuapp/tosu/wiki) documents the compatibility schemas and filter examples.

Command control queues and the shared relay mailbox each hold at most 16 frames/events. Relay saturation drops new refresh events; a recipient whose control queue is full is disconnected without blocking other recipients. Filter input is limited to 256 nodes and eight nesting levels, with equivalent filters deduplicated before projection.

Filtered compatibility streams share identical ordered filter plans only within a broadcast pass. The shared cache retains at most 64 variants and 1 MiB of payload bytes; exceeding either limit does not reject a filter, but encodes it without retaining it for reuse. Projection preserves requested ordering and the existing wire schemas.

The selected-field endpoint expects one text message within five seconds: a JSON array of 1–256 non-overlapping RFC 6901 JSON Pointers into the `/json/v2` document. A subscription such as `["/keys","/beatmap/mode","/state/number"]` receives compact initial `[0, revision, [[index, value], ...]]` and update `[1, revision, [[changedIndex, value], ...], [removedIndex, ...]]` frames. Invalid filters receive `[2, code, message]`. The selection is immutable: any later text message, including an application-level `["ping", nonce]`, receives `[2, 5, "subscription already established"]` and closes the connection with code `1008`. Keepalive uses WebSocket control frames only; vosu answers each Ping with a Pong. Reader sampling follows the [selected fields' producing lanes and activity](#renderer-state-v2): slow identity/settings/metadata selectors do not accelerate reads, gameplay-only fields accelerate only during play, and the live song-select clock can request precise sampling outside play. Only demanded active lanes use the smaller of their configured interval and `V2_SUBSCRIPTION_DEBOUNCE`; HTTP and compatibility WebSocket payloads retain their API cadence, except selection changes and resets are promoted immediately. Frames are sent only when a selected value changes, at most once per `V2_SUBSCRIPTION_DEBOUNCE` per client, and always carry the latest snapshot. Faster sampling costs CPU only in the requested lanes; a precise-only selection does not accelerate regular memory reads or live pp calculation.

Selected-field delivery is latest-only, not an append-only event log: a newer pending snapshot replaces an older unsent one. Consumers must use the received values as current state and cannot assume every intermediate revision or input transition will be delivered.

Control-frame backpressure is bounded too: an established selected-field connection whose pong queue is full closes with code `1011` rather than pinning the reader indefinitely.

## Diagnostic reports and logging

`/api/generateReport` uses native hardware probes, not a graphics runtime: Linux reports DRM/sysfs device metadata and Windows uses WMI. Missing names or unclassifiable device types are reported explicitly rather than guessed.

Reports retain the ten newest log files, with up to 1 MiB and 10,000 content lines per file and 8 MiB and 100,000 content lines overall. Visible truncation notices identify omitted content; HTML remains escaped. Report capture and counter-directory discovery run in bounded blocking workers.

Console and file logs share a bounded ordered queue. Saturation drops newest records rather than blocking the producer. `flush` and shutdown wait for accepted output; a blocked console no longer occupies an async runtime worker. Configuration persistence and reload hooks likewise execute outside async runtime workers while retaining their single-owner ordering and atomic replacement.

## Contributing and maintainer documentation

Start with [CONTRIBUTING.md](CONTRIBUTING.md) for setup, checks, and pull-request expectations. The maintained `docs/` directory is ordinary reviewed Markdown. Update affected documentation in the implementation PR; no generator, scheduled inference, or whole-tree post-merge rewrite is required. CI runs `python scripts/check-doc-links.py` even for documentation-only changes and publishes the stable `CI required` status.

| Document | Scope |
| --- | --- |
| [Contributing](CONTRIBUTING.md) | Development setup, local checks, and pull requests |
| [Maintainer documentation](docs/index.md) | Navigation, information boundaries, and review conventions |
| [Architecture](docs/architecture/index.md) | Runtime boundaries, flows, deployment, and risks |
| [Decision records](docs/decisions/index.md) | Durable design choices and their consequences |
| [Benchmarks](docs/testing/benchmarks.md) | Criterion coverage, method, results, and limitations |
| [Testing and verification](docs/testing/verification.md) | Change-specific evidence and release checks |

## License

Project-authored code and documentation are dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option, where the respective contributors have licensing authority. vosu began as a Rust port of [tosu](https://github.com/tosuapp/tosu); tosu's copyright holder has permitted its tosu-derived code to be distributed under the same MIT OR Apache-2.0 terms, with tosu credited. The release archive retains the root [`LICENSE`](LICENSE) member. Bundled assets, dependencies, and provenance-pinned material remain under their applicable terms; see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) and [`crates/vosu-pp/THIRD_PARTY_LICENSES.txt`](crates/vosu-pp/THIRD_PARTY_LICENSES.txt). The root release license includes the six retained calculator MIT texts. The [provenance audit](docs/provenance-audit.md) records source matches, the relicensing permission, and the calculator questions that remain open.

vosu is an 18-LAB product. The vosu logo and icons are © 2026 sxlvia under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/); the terms are in the "Branding assets" section of [`LICENSE`](LICENSE), and the usage rules are in [branding](docs/branding.md). Showing them while using or talking about vosu (streams, videos, screenshots) needs no credit.
