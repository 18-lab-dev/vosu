<div align="center">

<img src="assets/branding/vosu-banner.svg" alt="Vosu — Native osu! telemetry, counters, and compatible APIs in Rust." width="100%">

<p>
  <a href="rust-toolchain.toml"><img alt="Rust 1.96.1" src="https://img.shields.io/badge/Rust-1.96.1-000000?style=flat-square"></a>
  <a href="#platform-support"><img alt="Linux: first-class" src="https://img.shields.io/badge/Linux-first--class-FCC624?style=flat-square"></a>
  <a href="#platform-support"><img alt="Windows: supported" src="https://img.shields.io/badge/Windows-supported-0078D4?style=flat-square"></a>
  <a href="LICENSE"><img alt="License: LGPL-3.0-only" src="https://img.shields.io/badge/License-LGPL--3.0--only-6e5494?style=flat-square"></a>
</p>

</div>

> A vibe-coded, ground-up Rust rewrite of upstream [tosu](https://github.com/tosuapp/tosu), with first-class Linux support, measured hot paths, and a repository designed to work well with coding agents.

The daemon was written from scratch in Rust, and its embedded dashboard is a first-party vanilla HTML, CSS, and JavaScript interface. The upstream project defines the compatibility target and calculator provenance; it is not a runtime dependency or a translated codebase.

`vosu` reads live state from osu!stable and osu!lazer, then serves the dashboard, counters, reports, and compatible HTTP/WebSocket APIs. It is one native daemon: no Node.js, Electron, .NET, or C# runtime is required.

This build does not provide the former Electron/asdf-overlay in-game overlay or an embedded replacement. External overlay software can consume the documented HTTP and WebSocket APIs; [ADR 0008](openwiki/decisions/0008-external-in-game-overlays.md) records this product boundary.

## Why this build

| | |
| --- | --- |
| **Native runtime** | A single Rust daemon with embedded dashboard assets and offsets. |
| **Verified compatibility** | gosuMemory v1, streamCompanion, and vosu v2 HTTP/WebSocket behavior are checked against the pinned upstream tosu reference with live parity captures. |
| **Linux support** | Native osu!lazer and osu!stable through Wine are supported alongside Windows stable and lazer. |
| **Measured performance** | Criterion covers memory scanning, snapshot reads, calculations, serialization, caching, and publication. Results and limits are recorded in the [benchmark guide](openwiki/testing/benchmarks.md). |
| **Agent-ready repository** | A pinned toolchain, explicit source ownership, runnable checks, architecture notes, and decision records keep automated changes reviewable. Start with [AGENTS.md](AGENTS.md). |

## Install

### Portable archive

Download `vosu-linux-v0.2.0.zip` or `vosu-windows-v0.2.0.zip` from the matching [GitHub release](https://github.com/shiruviaa/vosu/releases), then:

1. Extract the complete archive into a writable directory. Its root contains `vosu` or `vosu.exe`, plus `LICENSE`.
2. On Linux, restore the executable bit if your archive tool removed it: `chmod +x vosu`.
3. Run `./vosu` on Linux or `vosu.exe` on Windows.
4. Open <http://127.0.0.1:24050/>. By default, vosu opens the dashboard after startup.

Do not run the Windows executable directly from an archive or a OneDrive-controlled directory. The updater cannot provide safe replacement and rollback in those locations.

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

`ENABLE_AUTOUPDATE=true` follows stable `shiruviaa/vosu` releases. The updater verifies the selected platform archive against its published SHA-256 digest, stages exactly one root executable, and uses a crash-recoverable helper and rollback flow.

`GET /api/runUpdates` starts the same update path. Set `ENABLE_AUTOUPDATE=false` or launch with `--update=false` to disable the startup check. `--update=true` enables it for one launch.

## Command-line options

The public command flags are standalone actions:

```text
-h, --help
-s, --setup
-g, --generate-offsets
```

`--help` prints usage. On Linux, `--generate-offsets` finds exactly one running
64-bit osu!lazer client, invokes the lazer offset generator in live-process mode,
and atomically writes validated offsets for that client's version to the vosu
offset cache: `$XDG_CACHE_HOME/vosu/offsets/<version>.json`, defaulting to
`$HOME/.cache/vosu/offsets/<version>.json`. Set `VOSU_LAZER_OFFSET_GENERATOR` to
the built generator path when it is not beside the vosu executable or under the
repository's standard tool output path. Set `VOSU_LAZER_OFFSET_DAC` when the
client's matching `libmscordaccore.so` is not beside its executable. The
command requires the Linux generator and does not use the embedded offsets as
generated output. On Linux, `--setup` probes the process-event backends
compiled into the current executable and installs the capabilities they
require. Each action must be the executable's only argument.

Insufficient setup authority is an error. Run `sudo vosu --setup`, or invoke it as a user with equivalent authority to set file capabilities; do not run the ordinary daemon as root.

`--generate-offsets` is a Linux-only maintainer action. A release archive
contains the vosu daemon and `LICENSE`, not the managed offset generator. The
action attaches to one already-running native 64-bit osu!lazer client; it does
not launch osu! and it does not modify the daemon's embedded offsets.

There is no separate prebuilt generator package in the current release. Build
the tool from a matching vosu source checkout as shown below.

#### After installing the release binary

Install prerequisites before running the action:

- a native 64-bit osu!lazer client, already running as the same desktop user;
- a DAC built for the exact CLR loaded by the client; the client's bundled
  `libmscordaccore.so` is not necessarily the DAC for osu!'s Satori runtime;
- the .NET 8 SDK and access to NuGet to restore the generator's locked package;
- Git, unless a vosu source checkout is already available.

The SDK is required; installing only a .NET runtime is insufficient. On Arch
Linux, install it with:

```sh
sudo pacman -S dotnet-sdk-8.0
dotnet --list-sdks
```

Build the generator from a checkout, then point the installed daemon at its
absolute path:

```sh
git clone https://github.com/shiruviaa/vosu.git
cd vosu
dotnet restore tools/lazer-offset-generator/lazer-offset-generator.csproj \
  --source https://api.nuget.org/v3/index.json --locked-mode
dotnet build tools/lazer-offset-generator/lazer-offset-generator.csproj \
  --configuration Release --no-restore
export VOSU_LAZER_OFFSET_GENERATOR="$PWD/tools/lazer-offset-generator/bin/Release/net8.0/lazer-offset-generator"
sudo setcap cap_sys_ptrace=ep "$VOSU_LAZER_OFFSET_GENERATOR"
getcap "$VOSU_LAZER_OFFSET_GENERATOR"
/path/to/vosu --generate-offsets
```

`vosu --setup` grants capabilities to the daemon executable, but the
generator is a separate process and must have `CAP_SYS_PTRACE` itself. Do not
run the daemon as root. If the daemon's attach still fails, run
`sudo /path/to/vosu --setup` once and retry as the desktop user.

For osu!lazer builds using ppy/Satori, use the patched DAC produced by the
reproducible workflow below rather than assuming the AppImage's bundled DAC
matches. Set it explicitly, for example:

```sh
export VOSU_LAZER_OFFSET_DAC=/tmp/satori-source/artifacts/bin/coreclr/linux.x64.Release/libmscordaccore.so
```

#### From a cloned repository

The repository workflow needs the Rust, native-library, and .NET prerequisites
listed in [Requirements](#requirements). From the repository root:

```sh
dotnet restore tools/lazer-offset-generator/lazer-offset-generator.csproj \
  --source https://api.nuget.org/v3/index.json --locked-mode
dotnet build tools/lazer-offset-generator/lazer-offset-generator.csproj \
  --configuration Release --no-restore
cargo build --locked --no-default-features
export VOSU_LAZER_OFFSET_GENERATOR="$PWD/tools/lazer-offset-generator/bin/Release/net8.0/lazer-offset-generator"
sudo setcap cap_sys_ptrace=ep target/debug/vosu
sudo setcap cap_sys_ptrace=ep "$VOSU_LAZER_OFFSET_GENERATOR"
cargo run --locked --no-default-features -- --generate-offsets
```

`cargo run` builds the daemon with the pinned Rust toolchain before invoking
the action. The generated file is stored in the vosu offset cache, not in
the repository checkout. The output is not copied to `assets/offsets.json`
automatically; updating the embedded repository asset requires the
reproducible workflow below and its provenance/checksum review.

If no client, generator, or DAC can be found, or if attach, version, or JSON
validation fails, the command returns an error and leaves an existing
`<version>.json` cache entry unchanged. Close extra osu!lazer instances, ensure
the daemon and client run as the same user, and check that the client exposes
`osu!.deps.json` before retrying. A `PTRACE_ATTACH` error from ClrMD usually
means the generator itself lacks `CAP_SYS_PTRACE`; reapply the `setcap`
command above after rebuilding it. Only after both executables have the
required capability should you adjust a host ptrace policy, and never by
running vosu as root.
Live-process mode can only resolve types that the target runtime has
constructed. If a client has not visited every required screen, use the
target-host workflow below with the same client assembly directory and write
the validated `<version>.json` result to the offset cache.

If ClrMD reports `Mismatched dac` with `expected: 0.0`, the selected DAC is
not the patched Satori DAC for the loaded `libcoreclr.so`. Set
`VOSU_LAZER_OFFSET_DAC` to the DAC produced by the reproducible workflow
above; do not bypass this mismatch.

## Build from source

### Requirements

- Rust **1.96.1**, also pinned in `rust-toolchain.toml`
- a native toolchain for `x86_64-unknown-linux-gnu` or `x86_64-pc-windows-msvc`
- Linux: a GNU C toolchain, Clang, pkg-config, libelf/zlib development files, and the Vulkan/OpenGL loader development files needed by `wgpu`
- Windows: MSVC build tools and the Windows SDK

The dashboard and embedded offsets are vendored build inputs. No frontend or
managed-runtime toolchain is needed for the daemon build. The live
offset-generation action additionally requires the .NET 8 SDK and a locked
NuGet restore; the reproducible asset workflow below has further maintainer
tool prerequisites.

```sh
cargo build --locked
cargo run --locked
cargo install --path . --locked
```

Default Linux builds include `linux-process-events`, so a binary installed by the command above can probe eBPF and CN_PROC during `sudo vosu --setup`. Pass `--no-default-features` explicitly only when building a polling-only binary.

Production builds:

```sh
# Linux
cargo build --release --locked --no-default-features --features linux-process-events --target x86_64-unknown-linux-gnu

# Windows, from Developer PowerShell
cargo build --release --locked --no-default-features --target x86_64-pc-windows-msvc
```

The `test-harness` feature is development-only. `build.rs` rejects it in release profiles; it is not a production switch.

### Reproducibly update the vendored osu!lazer offsets asset

The live workflow above is for a currently running client and local output.
To replace `assets/offsets.json` in the repository, use the exact source and
runtime workflow below. It records the release, runtime, DAC, and generator
inputs needed for a reviewable update; after generation, review the asset
diff and update its checksum and provenance metadata as described below.

#### Build only the patched Satori DAC

The AppImage's bundled DAC is the Microsoft runtime DAC and may not match
osu!'s patched Satori `libcoreclr.so`. For live generation, build the pinned
Satori DAC first. The following is the DAC-only portion of the workflow; run it
from the repository root after installing the prerequisites above:

```sh
set -eu
SATORI_ROOT=/tmp/satori-source
SATORI_REVISION=098d0ed17cf8f965d79be6e3d870a895a6dbbe04
SATORI_DAC_PATCH="$PWD/tools/lazer-offset-generator/patches/0001-guard-satori-gc-heap-data.patch"
SATORI_DAC_PATCH_SHA256=e0aaf89fff93b05dad0a4fc272683cbc852a3f51fd8d5b27ac8d91bafd3a597a

git clone --branch 2025.807.0 --depth 1 https://github.com/ppy/Satori.git "$SATORI_ROOT"
git -C "$SATORI_ROOT" checkout --detach "$SATORI_REVISION"
printf '%s  %s\n' "$SATORI_DAC_PATCH_SHA256" "$SATORI_DAC_PATCH" |
  sha256sum --check --strict
git -C "$SATORI_ROOT" apply --check "$SATORI_DAC_PATCH"
git -C "$SATORI_ROOT" apply "$SATORI_DAC_PATCH"
(
  cd "$SATORI_ROOT"
  ./build.sh clr.nativeprereqs -c Release
  printf 'static char sccsid[] __attribute__((used)) = "@(#)Version 0.0 @Commit: %s @Patch: %s";\n' \
    "$SATORI_REVISION" "$SATORI_DAC_PATCH_SHA256" > artifacts/obj/_version.c
  ./src/coreclr/build-runtime.sh -x64 -os linux -release -component debug
)
export VOSU_LAZER_OFFSET_DAC="$SATORI_ROOT/artifacts/bin/coreclr/linux.x64.Release/libmscordaccore.so"
test -f "$VOSU_LAZER_OFFSET_DAC"
```

Then rebuild the generator, reapply its capability, and run `vosu -g` as
described above. Do not point `VOSU_LAZER_OFFSET_DAC` at the AppImage's
`libmscordaccore.so` when the loaded `libcoreclr.so` is the pinned Satori build.


When a lazer release is not yet available from either offset endpoint, the Linux x64 maintainer tools can derive the embedded asset from an exact release source checkout under the release's exact runtime. They require the .NET 8 SDK, `curl`, `bsdtar`, binutils, zlib development headers, an executable `/dev/shm` tmpfs, and the [Satori Linux build prerequisites](https://github.com/ppy/Satori/blob/098d0ed17cf8f965d79be6e3d870a895a6dbbe04/.github/workflows/build.yml) only while generating offsets; vosu builds and releases remain Rust-only.

```sh
set -eu
VOSU_ROOT=$(pwd -P)
OSU_TAG=2026.726.0-lazer
OSU_VERSION=2026.726.0
OSU_REVISION=5da71008b082d1a77e4bb301dc98886f1f24b895
OSU_APPIMAGE=/tmp/osu-2026.726.0-lazer.AppImage
OSU_APPIMAGE_SHA256=3d3028609543dffd037b00652603f75928515100b6245bc929a9dc46fd3b29a0
SATORI_TAG=2025.807.0
SATORI_REVISION=098d0ed17cf8f965d79be6e3d870a895a6dbbe04
SATORI_DAC_PATCH="$VOSU_ROOT/tools/lazer-offset-generator/patches/0001-guard-satori-gc-heap-data.patch"
SATORI_DAC_PATCH_SHA256=e0aaf89fff93b05dad0a4fc272683cbc852a3f51fd8d5b27ac8d91bafd3a597a
SATORI_ARCHIVE=/tmp/satori-2025.807.0-linux-x64.zip
SATORI_ARCHIVE_SHA256=aef780b177f77caf463d986cdb030366866bb3502039473dcc71dd2bdeb68fd2
SATORI_CORELIB_SHA256=73c6ae7ff2ed535a249539f78731fc4412cf6c263ed9692fcf035f2cd01f96fa
SATORI_CLRJIT_SHA256=3d58119a829d10d20f9fc31f2144f9202af19c93b412d5a5eeeeeba4b35ebb3e
SATORI_CORECLR_SHA256=dcc631eda8fdb34d06956697e9709b8d828306eb73ac3187b5058180e1af615e
SATORI_CORECLR_BUILD_ID=924f6642f039382780dd0ee782477271d8093084
RUNTIME_PACK=/tmp/microsoft.netcore.app.runtime.linux-x64.8.0.29.nupkg
RUNTIME_PACK_SHA256=780e71f4d31f0a0e8985198e28aa9f04bf3e4f0b505bfd4e36f25e00d3c93850
HOST_PACK=/tmp/microsoft.netcore.app.host.linux-x64.8.0.29.nupkg
HOST_PACK_SHA256=4ff7979334f75774f561ab1cd5c519fe32ce589637ce9fc6c494c24f6e209c0c
REF_PACK=/tmp/microsoft.netcore.app.ref.8.0.29.nupkg
REF_PACK_SHA256=77102e11453554e1254380a92fd1cb855e0a545b510c07194bc5b4193d838ba8
RUNTIME_PACK_SOURCE=/tmp/vosu-runtime-pack-source
RUNTIME_PACKAGES=/tmp/vosu-runtime-packages
TARGETING_PACK_ROOT=/tmp/vosu-empty-targeting-packs
DOTNET_CLI_HOME=/tmp/vosu-dotnet-cli-home
WORKLOAD_PACK_ROOTS=/tmp/vosu-empty-workload-packs
printf '%s  %s\n' "$SATORI_DAC_PATCH_SHA256" "$SATORI_DAC_PATCH" |
  sha256sum --check --strict

rm -rf /tmp/osu-offset-source
git clone --branch "$OSU_TAG" --depth 1 https://github.com/ppy/osu.git /tmp/osu-offset-source
test "$(git -C /tmp/osu-offset-source rev-parse "${OSU_TAG}^{commit}")" = "$OSU_REVISION"
git -C /tmp/osu-offset-source checkout --detach "$OSU_REVISION"
test "$(git -C /tmp/osu-offset-source rev-parse HEAD)" = "$OSU_REVISION"

curl --fail --location --output "$OSU_APPIMAGE" \
  "https://github.com/ppy/osu/releases/download/$OSU_TAG/osu.AppImage"
printf '%s  %s\n' "$OSU_APPIMAGE_SHA256" "$OSU_APPIMAGE" |
  sha256sum --check --strict
chmod u+x "$OSU_APPIMAGE"
rm -rf /tmp/osu-appimage-runtime
mkdir -p /tmp/osu-appimage-runtime
(
  cd /tmp/osu-appimage-runtime
  "$OSU_APPIMAGE" --appimage-extract 'usr/bin/*.dll'
  "$OSU_APPIMAGE" --appimage-extract usr/bin/libclrjit.so
  "$OSU_APPIMAGE" --appimage-extract usr/bin/libcoreclr.so
  "$OSU_APPIMAGE" --appimage-extract 'usr/bin/osu!.runtimeconfig.json'
  "$OSU_APPIMAGE" --appimage-extract 'usr/bin/osu!.deps.json'
)
OSU_RUNTIME_DIR=/tmp/osu-appimage-runtime/squashfs-root/usr/bin
OSU_RUNTIME_CONFIG="$OSU_RUNTIME_DIR/"'osu!.runtimeconfig.json'
OSU_DEPS="$OSU_RUNTIME_DIR/"'osu!.deps.json'
OSU_ASSEMBLY_DIR=/tmp/osu-appimage-assemblies
rm -rf "$OSU_ASSEMBLY_DIR"
mkdir -p "$OSU_ASSEMBLY_DIR"
cp "$OSU_RUNTIME_DIR"/*.dll "$OSU_ASSEMBLY_DIR/"
rm "$OSU_ASSEMBLY_DIR/System.Private.CoreLib.dll"
test -f "$OSU_ASSEMBLY_DIR/"'osu!.dll'
test -f "$OSU_ASSEMBLY_DIR/osu.Game.dll"
test -f "$OSU_ASSEMBLY_DIR/osu.Framework.dll"
printf '%s  %s\n' \
  "$SATORI_CORELIB_SHA256" "$OSU_RUNTIME_DIR/System.Private.CoreLib.dll" \
  "$SATORI_CLRJIT_SHA256" "$OSU_RUNTIME_DIR/libclrjit.so" \
  "$SATORI_CORECLR_SHA256" "$OSU_RUNTIME_DIR/libcoreclr.so" |
  sha256sum --check --strict
readelf --notes "$OSU_RUNTIME_DIR/libcoreclr.so" |
  grep --fixed-strings "Build ID: $SATORI_CORECLR_BUILD_ID"
grep --fixed-strings '"version": "8.0.29"' "$OSU_RUNTIME_CONFIG"
grep --fixed-strings \
  '"runtimepack.Microsoft.NETCore.App.Runtime.linux-x64": "8.0.29"' \
  "$OSU_DEPS"
grep -A 2 --fixed-strings '"System.Private.CoreLib.dll"' \
  "$OSU_DEPS" |
  grep --fixed-strings '"fileVersion": "8.0.2926.32403"'

curl --fail --location --output "$SATORI_ARCHIVE" \
  "https://github.com/ppy/Satori/releases/download/$SATORI_TAG/linux-x64.zip"
printf '%s  %s\n' "$SATORI_ARCHIVE_SHA256" "$SATORI_ARCHIVE" |
  sha256sum --check --strict
rm -rf /tmp/satori-runtime
mkdir -p /tmp/satori-runtime
bsdtar -xf "$SATORI_ARCHIVE" -C /tmp/satori-runtime
SATORI_RUNTIME_DIR=/tmp/satori-runtime/linux-x64
printf '%s  %s\n' \
  "$SATORI_CORELIB_SHA256" "$SATORI_RUNTIME_DIR/System.Private.CoreLib.dll" \
  "$SATORI_CLRJIT_SHA256" "$SATORI_RUNTIME_DIR/libclrjit.so" \
  "$SATORI_CORECLR_SHA256" "$SATORI_RUNTIME_DIR/libcoreclr.so" |
  sha256sum --check --strict
cmp --silent "$SATORI_RUNTIME_DIR/System.Private.CoreLib.dll" \
  "$OSU_RUNTIME_DIR/System.Private.CoreLib.dll"
cmp --silent "$SATORI_RUNTIME_DIR/libclrjit.so" "$OSU_RUNTIME_DIR/libclrjit.so"
cmp --silent "$SATORI_RUNTIME_DIR/libcoreclr.so" "$OSU_RUNTIME_DIR/libcoreclr.so"

rm -rf /tmp/satori-source
git clone --branch "$SATORI_TAG" --depth 1 \
  https://github.com/ppy/Satori.git /tmp/satori-source
test "$(git -C /tmp/satori-source rev-parse "${SATORI_TAG}^{commit}")" = "$SATORI_REVISION"
git -C /tmp/satori-source checkout --detach "$SATORI_REVISION"
test "$(git -C /tmp/satori-source rev-parse HEAD)" = "$SATORI_REVISION"
git -C /tmp/satori-source apply --check "$SATORI_DAC_PATCH"
git -C /tmp/satori-source apply "$SATORI_DAC_PATCH"
(
  cd /tmp/satori-source
  ./build.sh clr.nativeprereqs -c Release
  printf 'static char sccsid[] __attribute__((used)) = "@(#)Version 0.0 @Commit: %s @Patch: %s";\n' \
    "$SATORI_REVISION" "$SATORI_DAC_PATCH_SHA256" > artifacts/obj/_version.c
  ./src/coreclr/build-runtime.sh -x64 -os linux -release -component debug
)
SATORI_DAC=/tmp/satori-source/artifacts/bin/coreclr/linux.x64.Release/libmscordaccore.so
test -f "$SATORI_DAC"
strings "$SATORI_DAC" |
  grep --fixed-strings \
    "@(#)Version 0.0 @Commit: $SATORI_REVISION @Patch: $SATORI_DAC_PATCH_SHA256"

curl --fail --location --output "$RUNTIME_PACK" \
  "https://api.nuget.org/v3-flatcontainer/microsoft.netcore.app.runtime.linux-x64/8.0.29/microsoft.netcore.app.runtime.linux-x64.8.0.29.nupkg"
curl --fail --location --output "$HOST_PACK" \
  "https://api.nuget.org/v3-flatcontainer/microsoft.netcore.app.host.linux-x64/8.0.29/microsoft.netcore.app.host.linux-x64.8.0.29.nupkg"
curl --fail --location --output "$REF_PACK" \
  "https://api.nuget.org/v3-flatcontainer/microsoft.netcore.app.ref/8.0.29/microsoft.netcore.app.ref.8.0.29.nupkg"
printf '%s  %s\n' \
  "$RUNTIME_PACK_SHA256" "$RUNTIME_PACK" \
  "$HOST_PACK_SHA256" "$HOST_PACK" \
  "$REF_PACK_SHA256" "$REF_PACK" |
  sha256sum --check --strict
rm -rf "$RUNTIME_PACK_SOURCE" "$RUNTIME_PACKAGES" "$TARGETING_PACK_ROOT" \
  "$DOTNET_CLI_HOME" "$WORKLOAD_PACK_ROOTS"
mkdir -p "$RUNTIME_PACK_SOURCE" "$RUNTIME_PACKAGES" "$TARGETING_PACK_ROOT" \
  "$DOTNET_CLI_HOME" "$WORKLOAD_PACK_ROOTS"
install -m 0644 "$RUNTIME_PACK" "$RUNTIME_PACK_SOURCE/"
install -m 0644 "$HOST_PACK" "$RUNTIME_PACK_SOURCE/"
install -m 0644 "$REF_PACK" "$RUNTIME_PACK_SOURCE/"

TARGET_PROJECT=tools/lazer-offset-target/lazer-offset-target.csproj
TARGET_PUBLISH=tools/lazer-offset-target/bin/Release/net8.0/linux-x64/publish
rm -rf "$TARGET_PUBLISH" tools/lazer-offset-target/obj
DOTNET_CLI_HOME="$DOTNET_CLI_HOME" \
DOTNET_MULTILEVEL_LOOKUP=0 \
WORKLOAD_PACK_ROOTS="$WORKLOAD_PACK_ROOTS" \
NUGET_PACKAGES="$RUNTIME_PACKAGES" \
  dotnet publish "$TARGET_PROJECT" \
  --configuration Release \
  --runtime linux-x64 \
  --self-contained true \
  --source "$RUNTIME_PACK_SOURCE" \
  --no-cache \
  -p:RuntimeFrameworkVersion=8.0.29 \
  -p:NetCoreTargetingPackRoot="$TARGETING_PACK_ROOT" \
  -p:WORKLOAD_PACK_ROOTS="$WORKLOAD_PACK_ROOTS" \
  -p:RestorePackagesPath="$RUNTIME_PACKAGES" \
  -p:RestoreFallbackFolders= \
  -p:DisableTransitiveFrameworkReferenceDownloads=true
install -m 0644 "$SATORI_RUNTIME_DIR/System.Private.CoreLib.dll" \
  "$TARGET_PUBLISH/System.Private.CoreLib.dll"
install -m 0755 "$SATORI_RUNTIME_DIR/libclrjit.so" "$TARGET_PUBLISH/libclrjit.so"
install -m 0755 "$SATORI_RUNTIME_DIR/libcoreclr.so" "$TARGET_PUBLISH/libcoreclr.so"
printf '%s  %s\n' \
  "$SATORI_CORELIB_SHA256" "$TARGET_PUBLISH/System.Private.CoreLib.dll" \
  "$SATORI_CLRJIT_SHA256" "$TARGET_PUBLISH/libclrjit.so" \
  "$SATORI_CORECLR_SHA256" "$TARGET_PUBLISH/libcoreclr.so" |
  sha256sum --check --strict
GENERATOR_PACKAGES=$(mktemp -d /tmp/vosu-lazer-offset-generator-packages.XXXXXX)
NUGET_PACKAGES="$GENERATOR_PACKAGES" \
  dotnet restore tools/lazer-offset-generator/lazer-offset-generator.csproj \
  --source https://api.nuget.org/v3/index.json \
  --no-cache \
  --locked-mode \
  -p:RestorePackagesPath="$GENERATOR_PACKAGES"
NUGET_PACKAGES="$GENERATOR_PACKAGES" \
  dotnet build tools/lazer-offset-generator/lazer-offset-generator.csproj \
  --configuration Release \
  --no-restore \
  -p:RestorePackagesPath="$GENERATOR_PACKAGES"
rm -rf "$GENERATOR_PACKAGES"
TARGET_RUNTIME=$(mktemp -d /dev/shm/vosu-lazer-offset-target.XXXXXX)
trap 'rm -rf "$TARGET_RUNTIME"' EXIT
test "$(stat -f -c %T "$TARGET_RUNTIME")" = tmpfs
cp -aL "$TARGET_PUBLISH/." "$TARGET_RUNTIME/"
printf '%s  %s\n' \
  "$SATORI_CORELIB_SHA256" "$TARGET_RUNTIME/System.Private.CoreLib.dll" \
  "$SATORI_CLRJIT_SHA256" "$TARGET_RUNTIME/libclrjit.so" \
  "$SATORI_CORECLR_SHA256" "$TARGET_RUNTIME/libcoreclr.so" |
  sha256sum --check --strict
"$TARGET_RUNTIME/lazer-offset-target" --help >/dev/null
```

The AppImage declares the Microsoft `linux-x64` runtime pack 8.0.29 and its dependency manifest records `System.Private.CoreLib.dll` file version `8.0.2926.32403`. osu!'s deployment then overlays ppy/Satori 2025.807.0, whose exact source reports product version 8.0.16, for `System.Private.CoreLib.dll`, `libclrjit.so`, and `libcoreclr.so`. The archive and per-file checks above prove those three Satori files are byte-identical to the official AppImage. Satori's non-official release gives the CoreCLR ELF a native marker of `42.42.42.42424`, while pinned ClrMD reports this stripped Linux runtime's comparable file version as the two-component `0.0`; the DAC therefore embeds `0.0`, the verified Satori commit, and the committed compatibility-patch hash. The stock Microsoft DAC is not a valid substitute for the Satori GC.

The commands restore the hash-verified Microsoft runtime, host, and reference packages through a fresh local NuGet source and `NUGET_PACKAGES` directory, with fallback and transitive framework-reference downloads disabled. Fresh empty targeting-pack and workload-pack roots, a fresh CLI home, and disabled multilevel lookup prevent the SDK from substituting installed or user-local packs. The target project also pins the `Microsoft.NETCore.App` targeting pack to 8.0.29, so SDK defaults cannot request another patch. The authoritative runtime identity is the AppImage-matched Satori CoreLib, JIT, and CoreCLR trio installed after publish and verified again by both tools.

The generator restore's committed lock file fixes the entire ClrMD dependency graph, and NuGet validates each restored package against the lock's `contentHash` while using only the explicit nuget.org source and fresh private package root.

Satori does not populate the standard GC DAC globals, but its unmodified `GetGCHeapData` and `GetGCHeapStaticData` implementations dereference them while ClrMD constructs a runtime. The pinned compatibility patch prevents those invalid reads: it reports Satori's single-heap topology with GC structures unavailable and returns `E_NOTIMPL` for the unsupported static heap query. The generator does not inspect managed heap objects; module, type, method-table, and field metadata still come from the matching Satori DAC. The generator rejects a DAC without the exact version, source-commit, and compatibility-patch markers and leaves ClrMD's version-mismatch check enabled.

Run the generator once. It launches the pinned target executable as its direct child and always terminates and reaps that child:

```sh
tools/lazer-offset-generator/bin/Release/net8.0/lazer-offset-generator \
  --target-host "$TARGET_RUNTIME/lazer-offset-target" \
  --dac "$SATORI_DAC" \
  --assembly-dir "$OSU_ASSEMBLY_DIR" \
  --template assets/offsets.json \
  --output assets/offsets.json
rm -rf "$TARGET_RUNTIME"
trap - EXIT
```

Before printing its ready line, the target host verifies Linux x64, Satori product version 8.0.16, and the exact loaded Satori CoreLib, JIT, and CoreCLR hashes. It then loads the hash-verified official AppImage's managed assemblies in an isolated load context and retains every template type's runtime handle; the staged assembly directory omits `System.Private.CoreLib.dll` so that framework identity comes only from the already verified target runtime. It does not inspect fields or compute layouts. The generator explicitly loads the matching source-built Satori DAC with the committed compatibility patch through the pinned Microsoft.Diagnostics.Runtime (ClrMD) package. ClrMD alone resolves loaded type definitions, declaring types, inherited instance fields, field offsets, and the concrete `osu.Desktop.OsuGameDesktop` method-table word. The official AppImage does not construct every required state-specific method table during normal startup, so the target host activates those types under the exact Satori runtime while preserving the AppImage's managed assembly bytes.

The commands stage the complete target publish on a fresh executable tmpfs and re-hash the pinned runtime trio after copying. Before launch, the generator hashes all three files through held descriptors and records each device, inode, size, and nanosecond change time. The target requires those exact pre-launch stamps, binds each descriptor to `/proc/self/maps`, and verifies the hashes; after reaping the target, the generator verifies the held descriptors and hashes again. The change-time checks reject an in-place write even if the original bytes are restored, while strict device-and-inode matching rejects path or Btrfs-subvolume substitution. The generator copies the supplied DAC into a sealed anonymous file, validates the markers in that immutable snapshot, and gives ClrMD its `/proc/self/fd` path; validation and loading therefore use identical bytes. Generated JSON is kept in memory until target termination, reaping, and final runtime verification all succeed, so cleanup or identity failure cannot print or replace output.

On Linux, the generator keeps the target as its direct child for the entire ClrMD attach, preserving the relationship expected by Yama restricted-ptrace mode; ordinary ptrace eligibility still applies. Neither tool installs a target-side ptrace exception or changes system ptrace policy. A runtime-identity mismatch, DAC mismatch, missing or ambiguous type, or missing or ambiguous field fails without replacing the output. After reviewing the diff, update the `assets/offsets.json` entry in `assets/SHA256SUMS`, then record the exact osu! and Satori source revisions, release tags and URLs, input archive hashes, runtime identities, and generated JSON SHA-256 in `package.metadata.vosu` and `THIRD_PARTY_NOTICES.md`.

The JSON schema is also the embedded, cache, and remote offset interchange contract (`<version>.json`). Replacing this 7,892-byte asset with a packed format requires a separately versioned parser and endpoint migration; generation must not silently change the deployed format.

## Platform support

Run vosu as the same desktop user as osu!. Tournament and debug-client processes are filtered by command line.

| Platform | osu!stable | osu!lazer |
| --- | --- | --- |
| **Linux** | Wine process discovery and native memory reads. Set `VOSU_OSU_PATH=/absolute/path/to/osu!` if Wine does not expose the install directory. | The official executable or AppImage layout must expose `osu!.deps.json`. Sandbox policy must allow process discovery and memory reads. |
| **Windows** | Supported 32-bit processes are read through native Windows process-memory APIs. | A supported 64-bit process is required. Offsets are resolved from embedded data, the versioned cache, then the first-party `https://vosu.app/offsets` service and configured fallback services. |

On Linux, vosu can also read the osu! path from `${XDG_DATA_HOME:-$HOME/.local/share}/osuconfig/osupath`.

Official Linux archives statically include libbpf and dynamically use the system `libelf.so.1` and `libz.so.1` libraries.

On Linux, `process_vm_readv` must be permitted. During ordinary startup vosu checks the running process's effective capability set and warns if `CAP_SYS_PTRACE` is not effective, or if that check cannot be completed. The warning does not stop startup, but memory reads may fail. Configure the exact executable with:

```sh
sudo vosu --setup
```

`--setup` directly invokes a trusted, root-owned `setcap` executable and exits without starting the daemon. A normal build receives only `CAP_SYS_PTRACE` and reports polling as its available discovery backend. A build compiled with `linux-process-events` first attempts its maximum compiled capability set. When that succeeds, it re-executes itself to probe eBPF and CN_PROC independently, then retains `CAP_SYS_RESOURCE`, `CAP_PERFMON`, and `CAP_BPF` only when eBPF actually works. If the maximum set is unavailable, setup retries with `CAP_SYS_PTRACE`, marks eBPF unavailable, and still probes CN_PROC before reporting polling as the final fallback. CN_PROC requires no additional file capability. Probe failures and the final capability set are printed.

If `setcap` lacks authority, setup exits unsuccessfully without starting the daemon. Executable replacement, including an automatic update, removes file capabilities, so rerun `sudo vosu --setup` after replacing vosu.

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
POLL_RATE = 150.0
PRECISE_DATA_POLL_RATE = 10.0
SERVER_IP = "127.0.0.1"
SERVER_PORT = 24050.0
STATIC_FOLDER_PATH = "./static"
ALLOWED_IPS = "127.0.0.1,localhost,absolute"
```

At startup, malformed canonical TOML leaves every source file untouched. During watch reload, malformed TOML leaves the last-good runtime configuration and dependent resources active.

| Binding | Default | Meaning |
| --- | ---: | --- |
| `ENABLE_AUTOUPDATE` | `true` | Check for and install stable vosu releases. |
| `OPEN_DASHBOARD_ON_STARTUP` | `true` | Open the effective dashboard listener after startup. |
| `DEBUG_LOG` | `false` | Include debug-level file and console logging. |
| `CALCULATE_PP` | `true` | Enable live performance calculations. |
| `ENABLE_KEY_OVERLAY` | `true` | Publish key-overlay data in compatible schemas. |
| `POLL_RATE` | `150` | Regular state interval in milliseconds; minimum `100`. |
| `PRECISE_DATA_POLL_RATE` | `10` | Precise state interval in milliseconds; minimum `1`. |
| `V2_SUBSCRIPTION_DEBOUNCE` | `0` | Minimum interval in milliseconds between selected-field WebSocket frames; minimum `0`, fractional values supported. |
| `SHOW_MP_COMMANDS` | `false` | Include multiplayer commands where supported. |
| `READ_MANIA_SCROLL_SPEED` | `true` | Read mania scroll speed. |
| `SERVER_IP` | `127.0.0.1` | Listener hostname or address. |
| `SERVER_PORT` | `24050` | Listener port. |
| `STATIC_FOLDER_PATH` | `./static` | Counter root; the default maps to the data directory. |
| `ALLOWED_IPS` | `127.0.0.1,localhost,absolute` | Peer IP patterns and allowed Origin/Referer host tokens. |

Authorization requires an allowed network peer and, when supplied, an allowed `Origin` or `Referer`. A hostname token does not allow an arbitrary remote peer. Keep the default loopback listener unless remote access is intentionally configured and protected.


## Dashboard and counters

The dashboard at `/` discovers counters recursively below the static root. A counter normally contains `index.html`. `metadata.txt` can add a name, author links, description, version, and `WxH` size. An immediate `ignore.txt` excludes its directory from discovery, and directories named `node_modules` are never traversed. Mutation APIs accept counter names containing one normal path component.

A top-level directory symlink directly below the static root is an explicit local mount declaration and may target a directory outside that root; no `index.html` marker is required for authorization. Discovery follows that top-level alias but skips nested directory symlinks, which neither create nor traverse additional mount roots. Every requested file below the alias is still confined to the current alias target and streamed through a retained handle. Counter settings remain available through aliases, while final symlinked settings files are rejected and settings writes use atomic same-directory replacement. **Open Folder** follows a directory alias. **Delete** unlinks the alias itself without traversing or deleting its target.

Installed counter cards can be pinned; pinned cards stay above unpinned cards in their saved order and can be reordered by dragging. Shift-click a counter URL to open it in a new window instead of copying it. Press `/` outside an editable control to focus the dashboard search.

Counter settings support text, color, number, checkbox, button, options, commands, textarea, password, header entries. Each counter package owns its authoritative schema at `<static-root>/<counter>/settings.schema.toml`; only mutable values are stored centrally at `<config>/settings/<counter>.settings.values.toml`. `/settings` page handles daemon counter settings.

The counter catalog is served from `https://vosu.app/api.json`. Production counter downloads require HTTPS. Redirects are bounded; HTTPS-to-HTTP downgrades and loopback, private, or reserved destinations are rejected. ZIP paths, symlinks, entry counts, and expanded size are checked before same-filesystem replacement.

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

The v2 snapshot and matching WebSocket streams expose `beatmap.stats.timingWindows`. For osu! and taiko, temporal judgement windows are maximum absolute errors in milliseconds scaled to real time by `clockRate`. For osu!mania, gameplay rate is applied to the source timing window before floor-plus-half quantization, so the displayed base `perfect` window of 19.5 ms at 1.0x becomes 25.5 ms at 1.3x. For catch, only `"300"` is present and represents the spatial half-width of the catcher's centre in playfield units; unsupported judgements are omitted.

Hit-error fields use the same object shape across v1, streamCompanion, v2, and v2 precise: `{"all":[],"press":[],"release":[]}`. `all` preserves timing-event order, `press` contains note presses, and `release` contains osu!lazer mania hold-note releases. Stable reports every timing error in `all` and `press` and leaves `release` empty.

`/json/v2/precise` publishes low-latency input data at `PRECISE_DATA_POLL_RATE`. Its additive `keypresses` object contains the active ruleset in `mode` and every source-reported logical input in ordered `keys` entries (`name`, `isPressed`, `count`, `pressedAt`, and `releasedAt`) for stable and lazer; the legacy fixed `keys` object remains available. Edge timestamps are nullable gameplay milliseconds. They update only when the sampled state exposes that edge, so a count jump can report missed complete taps without fabricating timestamps.
Maintainers can run the zero-dependency harness with `cd tools/input-validation && uv run python run.py ...`. `capture` accounts for precise transitions and `scan-maps` selects long-note-heavy mania maps. `autoplay <map.osu>` and `live-autoplay <map.osu>` generate deterministic plans for osu!, taiko, catch, and mania, then inject them through Windows `SendInput` or Linux `/dev/uinput`; live mania plans additionally support bounded seeded timing jitter. Standard-mode and catch pointer injection requires `--playfield x,y,width,height`; Linux also requires the full compositor layout as `--desktop x,y,width,height`, not only the target monitor. Default controls are `Z` for osu!, `F,J,D,K` for taiko plan controls `DON_LEFT,DON_RIGHT,KAT_LEFT,KAT_RIGHT`, pointer-only for catch, and `D,F,J,K` for 4-key mania.

Maintainer workflow, evidence interpretation, troubleshooting, and maintenance triggers are documented in the [precise input validation guide](openwiki/testing/input-validation.md).

`navigate` follows keyboard paths from an unobstructed idle logo: lazer uses `P,P,P` for song select and `P,P,M,M` for multiplayer, while stable uses `P,P,P` and `P,P,M`. Navigation key-down onsets default to 100 ms apart. Optional song-select search text uses 1 ms key-down onset spacing. Use each command's `--help` for backend, timing, geometry, and key-map options.

All calculation parameters are optional: `path`, `lazer`, `mode` (`0` osu, `1` taiko, `2` catch, `3` mania), `ar`, `cs`, `hp`, `od`, `clockRate`, `passedObjects`, `combo`, `nMisses`, `n100`, `n300`, `n50`, `nGeki`, `nKatu`, `mods` (legacy bits or a JSON mod object/array), `acc`, `sliderEndHits`, `smallTickHits`, `largeTickHits`, and `hitresultPriority`.

### Current game files

- `GET /backgroundImage`
- `GET /files/beatmap/background`
- `GET /files/beatmap/audio`
- `GET /files/beatmap/file`
- `GET /Songs/{path}`
- `GET /files/beatmap/{path}`
- `GET /files/skin/{path}`; unavailable for lazer skins

File routes enforce root containment and support validated single HTTP byte ranges for media.

### Pages, counters, reports, and updates

- `GET /`, `/settings`, `/local-overlays`, `/available`, `/favicon.ico`
- `GET /assets/{path}` and the fallback counter-static route
- `POST /api/settingsSave`
- `GET /api/runUpdates`
- `GET /api/generateReport`
- `GET /api/counters/search/{query}`
- `GET /api/counters/download/{url}?name=<counter>&update=<value>`
- `GET /api/counters/open/{name}`
- `GET /api/counters/open-root` opens the configured counter/static root.
- `GET /api/counters/delete/{name}`
- `GET|POST /api/counters/settings/{name}`

POST bodies are capped at 1 MiB. Generated reports escape machine, configuration, counter, and log data before inserting it into HTML.

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

The selected-field endpoint expects one text message within five seconds: a JSON array of 1–256 non-overlapping RFC 6901 JSON Pointers into `/json/v2`. It sends compact initial `[0, revision, values]` and update `[1, revision, values, removedIds]` arrays; selector IDs are their zero-based request positions. The selection is immutable, so changing fields requires reconnecting. This endpoint has no command channel.

## Maintainer documentation

| Document | Scope |
| --- | --- |
| [Maintainer documentation](openwiki/index.md) | Navigation, information boundaries, and review conventions |
| [Architecture](openwiki/architecture/index.md) | Runtime boundaries, flows, deployment, and risks |
| [Decision records](openwiki/decisions/index.md) | Durable design choices and their consequences |
| [Benchmarks](openwiki/testing/benchmarks.md) | Criterion coverage, method, results, and limitations |
| [Testing and verification](openwiki/testing/verification.md) | Change-specific evidence and release checks |

## License

Licensed under [GNU LGPL v3.0 only](LICENSE). Bundled asset and dependency notices are in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Additional calculator notices are retained in [`crates/vosu-lazer-pp/THIRD_PARTY_LICENSES.txt`](crates/vosu-lazer-pp/THIRD_PARTY_LICENSES.txt).
