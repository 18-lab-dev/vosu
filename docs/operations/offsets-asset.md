---
type: Runbook
title: Reproduce the vendored osu!lazer offsets asset
description: Historical maintainer procedure that reproduced the 2026.726.0 assets/offsets.json from pinned osu!, Satori, and .NET inputs.
---

# Reproduce the vendored osu!lazer offsets asset

This is a maintainer procedure for `assets/offsets.json`. It is not a user or
contributor prerequisite: daemon builds and runtime offset generation never run it.

The mania per-key template adds `osu.Game.Rulesets.Mania.Objects.ManiaHitObject.column` with an explicitly unmeasured **zero** placeholder. The reader rejects zero and publishes no guessed columns. Measure this entry against the selected release before shipping per-key lazer support; the generator replaces numeric placeholders with measured instance offsets. Its inline `HitObjectProperty<int>` layout uses named constants for `backingBindable` (+0) and `backingValue` (+8), and reuses the existing bindable integer value layout (+0x40); confirm these live too. The assembly generator does not currently support measuring the interior of a constructed generic value type, so those constants are not fictitious CLR template fields.

The commands below reproduce the explicitly historical `2026.726.0` asset.
They document prior source, runtime, DAC, and generator inputs; they are not a
recipe for the currently retained `2026.921.0` asset. Provenance for the current
asset is listed in `THIRD_PARTY_NOTICES.md` and Cargo metadata. Do not replace
it from this historical recipe without independently measuring the exact
release/runtime and verifying the native reader.

The catch extension adds typed class fields `DrawableCatchRuleset.playfield` (inherited `Lazy<Playfield>`), `CatchPlayfield.<Catcher>k__BackingField`, `Catcher.<Dashing>k__BackingField`, and `osu.Framework.Graphics.Drawable.x`. Their asset values are **zero, unmeasured placeholders**; the catch reader does not use them until all are populated. The separate named `LAZY_VALUE_OFFSET = 0x18` for `System.Lazy<Playfield>._value` is also unverified and must be checked against the exact .NET runtime. Generate the four class fields with the existing offset generator, then verify the complete player → drawable ruleset → lazy playfield → playfield → catcher chain natively. The inherited `Drawable.x` is a float, not a `Vector2 position` field in framework 2026.921.0. X and Dashing are read together each precise cycle; the chain is refreshed at least every 100 ms and on screen change or read failure. Verify managed-object movement and retry/seek transitions live before release.

## Build only the patched Satori DAC

The AppImage's bundled DAC is the Microsoft runtime DAC and may not match
osu!'s patched Satori `libcoreclr.so`. For live generation, build the pinned
Satori DAC first. The following is the DAC-only portion of the workflow; run it
from the repository root after installing the Satori build prerequisites listed
under [Full historical reproduction](#full-historical-reproduction):

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

This DAC procedure belongs only to historical vendored-asset reproduction. It
does not participate in runtime offset generation: current generation uses the
embedded self-contained generator and the exact live PID.

**Historical recipe:** this checked-in procedure reproduces the prior
`2026.726.0` offsets asset using the .NET 8.0.29/Satori inputs listed in
`THIRD_PARTY_NOTICES.md`; it is not a recipe or provenance for the currently
retained `2026.921.0` asset. The current asset was measured from the
`2026.921.0-lazer` AppImage under its bundled .NET 10.0.12 runtime; its release,
source revision, AppImage hash, and asset hash are recorded in Cargo metadata
and the notices. Do not substitute this historical recipe when reproducing
current offsets; native live-reader verification remains required.

## Full historical reproduction

These Linux x64 maintainer tools can derive the vendored `assets/offsets.json`
asset from an exact release source checkout under that release's exact runtime.
The remaining commands below are the historical `2026.726.0` reproduction
procedure only. They require the .NET 8 SDK, `curl`, `bsdtar`, binutils, zlib
development headers, an executable `/dev/shm` tmpfs, and the [Satori Linux build
prerequisites](https://github.com/ppy/Satori/blob/098d0ed17cf8f965d79be6e3d870a895a6dbbe04/.github/workflows/build.yml);
these do not describe current `2026.921.0` provenance. vosu builds and releases
remain Rust-only apart from preparing the embedded generator bundle.

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

On Linux, the generator keeps the target as its direct child for the entire ClrMD attach, preserving the relationship expected by Yama restricted-ptrace mode; ordinary ptrace eligibility still applies. Neither tool installs a target-side ptrace exception or changes system ptrace policy. A runtime-identity mismatch, DAC mismatch, missing or ambiguous type, or missing or ambiguous field fails without replacing the output. After reviewing the diff, refresh `assets/SHA256SUMS` with `python scripts/check-asset-hashes.py --write`, then record the exact osu! and Satori source revisions, release tags and URLs, input archive hashes, and runtime identities in `package.metadata.vosu`, and the generated JSON SHA-256 in `THIRD_PARTY_NOTICES.md`.

The JSON schema is the embedded and cache offset interchange contract
(`<version>.json`). Replacing this 7,892-byte asset with a packed format would
require a separately versioned parser and cache migration; generation must not
silently change the deployed format.

