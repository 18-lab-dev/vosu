# Third-party notices

This file inventories third-party material retained in the vosu repository. vosu is a Rust implementation targeting API and behavioral compatibility with upstream tosu. It is an attribution aid, not a replacement for applicable license texts. Project-authored repository code and documentation are dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option, where the respective contributors have licensing authority; the release archive retains the root [`LICENSE`](LICENSE) member. Exact Rust package versions and checksums are authoritative in `Cargo.lock`.

First-party notice: `Copyright (c) 2026 sxlvia`, as confirmed by the maintainer for vosu work begun in 2026. The notice is repeated in `LICENSE-MIT`, the combined root `LICENSE`, and the matching calculator copies. It applies to project-authored material over which the respective contributors have licensing authority; it does not claim ownership of upstream work or resolve the source-provenance questions below.

## tosu compatibility and retained provenance

- **Upstream credit:** tosu and its contributors, including Mikhail Babynichev (Kotrik). The upstream [v4.25.0 package metadata](https://github.com/tosuapp/tosu/blob/9f42c88714da69ac2ba248126a189416c55d733c/package.json) names that author; its [license document](https://github.com/tosuapp/tosu/blob/9f42c88714da69ac2ba248126a189416c55d733c/LICENSE) retains `Copyright (C) 2023-2026 Mikhail Babynichev <https://kotrik.ru/>`. This is upstream attribution, not a first-party vosu copyright notice. Upstream declares `GPL-3.0` in `package.json` but supplies LGPL v3 text in `LICENSE`; the audit records this discrepancy rather than selecting or broadening its grant.
- **Historical imports:** the initial vosu snapshot retained tosu dashboard material and an LGPL-covered calculator wrapper. The dashboard replacement is recorded in [`af20c120`](https://github.com/shiruviaa/vosu/commit/af20c1201a21beecc7db88119a700b2964c1ab78); the calculator replacement is recorded in [`4971b568`](https://github.com/shiruviaa/vosu/commit/4971b568a8dd0e82c011214c74b4e3857fd182ae). Earlier snapshots retain their applicable licenses and notices.
- **Production review:** stable memory signature data matches upstream tosu, and some calculator structures and formulas overlap the former wrapper. A compatibility match alone does not determine copyright status or establish independent implementation. The [provenance audit](docs/provenance-audit.md) identifies the files requiring evidence before any licensing conclusion.
- **Current and historical pins:** `package.metadata.vosu.source-commit` and the current oracle lock use verified upstream revision `2359305a63f2aaebb201bc8d2eee58177f52590e`. Historical calculator records retain unresolved `1a67fd64cc38fe60306328dde0a429e3211ca4ed`; no authenticated mapping was recovered. Upstream `v4.25.0` at `9f42c88714da69ac2ba248126a189416c55d733c` is a separately identified comparison source, not a substitute for that historical pin.
- **Retained reference:** `tools/oracle/reference/lazer-calculator` preserves its own LGPL license, revision, and checksum manifest. The first-party dual-license grant does not relicense this reference or any copied or translated upstream material.

## Native live-calculation sources

The production `vosu-pp` calculator uses a Rust facade and MIT source forks. Its detailed source map and complete retained license texts are in [`crates/vosu-pp/THIRD_PARTY_LICENSES.txt`](crates/vosu-pp/THIRD_PARTY_LICENSES.txt). The repository records the replacement as clean-room work; the remaining provenance questions are listed above.

### rosu-pp 4.0.1 baseline

- **Material:** public calculation APIs and ruleset semantics in the local source fork, plus the separate crates.io dependency used by the legacy API
- **Copyright:** Copyright (c) 2021 Max
- **License:** MIT
- **Pinned source:** <https://github.com/MaxOhn/rosu-pp/tree/27a67242cad155a2767f5a25a423ca4ec3e3c274>
- **License source:** <https://github.com/MaxOhn/rosu-pp/blob/27a67242cad155a2767f5a25a423ca4ec3e3c274/LICENSE>

### rosu-map 0.2.1

- **Material:** map parsing and slider-path geometry used by the local map fork
- **Copyright:** Copyright (c) 2024 MaxOhn
- **License:** MIT
- **Pinned source:** <https://github.com/MaxOhn/rosu-map/tree/333dd2bea78e518d39c20932c828011f21d3305e>
- **License source:** <https://github.com/MaxOhn/rosu-map/blob/333dd2bea78e518d39c20932c828011f21d3305e/LICENSE>

### osu! ruleset semantics

- **Material:** score simulation, accuracy, hit-window, score-multiplier, mod, conversion, and live difficulty/performance semantics
- **Copyright:** Copyright (c) 2025 ppy Pty Ltd <contact@ppy.sh>
- **License:** MIT
- **Pinned source:** <https://github.com/ppy/osu/tree/97e94babde9d5ecbe574ecdd33509db27452cd0f>
- **License source:** <https://github.com/ppy/osu/blob/97e94babde9d5ecbe574ecdd33509db27452cd0f/LICENCE>
- **Current semantic revision:** <https://github.com/ppy/osu/tree/fe30ae7eea2beeb1f0d3a5ce8dde12bb78ca3f48>; its `LICENCE` bytes are identical to the baseline above, so the retained ppy text covers both revisions.

### ppy/osu-framework 2026.629.0

- **Material:** `PathApproximator`, `CircularArcProperties`, `Precision`, and vector-length semantics used to align slider geometry
- **Copyright:** Copyright (c) 2024 ppy Pty Ltd <contact@ppy.sh>
- **License:** MIT
- **Pinned source:** <https://github.com/ppy/osu-framework/tree/a0b031e7ff6c83d81da26fcbece7d25edbd222b4>
- **License source:** <https://github.com/ppy/osu-framework/blob/a0b031e7ff6c83d81da26fcbece7d25edbd222b4/LICENCE>
- **Verified license SHA-256:** `d07e3ce383a24bdc4665208e0ca3730e433ea1e8235b67a301a2b26335692c8e`

The existing calculator provenance record reports verification of all four
exact revisions and their MIT license files from external checkouts. The
production workspace uses the in-tree Rust implementation and its MIT forks;
the historical LGPL oracle directory is not a production build or runtime
input. That dependency boundary alone does not establish the origin of every
production source fragment.

### Calculator oracle provenance

The current generated calculator fixture and oracle lock target `osu-pp-calculator` 0.6.1 (`323bc04198b256b5a00f4e63c5c4cbb506f43cc5`), as well as tosu `2359305a63f2aaebb201bc8d2eee58177f52590e`. Historical baseline evidence remains distinct: `tools/oracle/calculator-diff-manifest.json`, `tools/oracle/reference/lazer-calculator` at `9a22c4c74f8e584fa64133a79a204dd045646bac`, inputs under `tools/oracle/inputs/osu-97e94bab`, and `csharp-test-evidence` retain their historical provenance and are not the current fixture's generator or external test evidence. None of these oracle artifacts is a production source, crate dependency, build input, or runtime dependency. The current workspace and `vosu-pp` package are dual-licensed under `MIT OR Apache-2.0`; this does not relicense the retained historical reference or any other separately licensed material.

## osu!lazer memory-offset generation input

- **Material:** `assets/offsets.json`, measured locally from the official osu! AppImage's managed assemblies using its bundled `Microsoft.NETCore.App 10.0.12` runtime. Its layout/template is identified in Cargo metadata as `tosuapp/tosu@2359305a63f2aaebb201bc8d2eee58177f52590e` plus local pause/clock fields; retain the upstream credit and license discrepancy above for that input. The current asset includes the `GameBaseVtable` signature derived by the owned target and was not generated with the historical Satori overlay/DAC recipe below. Asset-generation inputs are not distributed by vosu. The embedded generator bundle contains the owned generator/target payload, self-contained .NET runtime, and applicable attribution files; it does not contain game assemblies or the custom Satori DAC. Stock diagnostic libraries accompany the .NET runtime, but automatic assembly-only generation does not attach a DAC to the game.
- **Copyright:** Copyright (c) ppy Pty Ltd <contact@ppy.sh>
- **License:** MIT for the osu! source code; osu! bundled resources remain subject to their upstream terms
- **osu! version:** `2026.921.0`
- **Release tag:** `2026.921.0-lazer`
- **Source revision:** [`aaa75b7c0a9f21d558ce84cd0ffac6c739bad8e7`](https://github.com/ppy/osu/tree/aaa75b7c0a9f21d558ce84cd0ffac6c739bad8e7)
- **Release:** <https://github.com/ppy/osu/releases/tag/2026.921.0-lazer>
- **Verification AppImage:** <https://github.com/ppy/osu/releases/download/2026.921.0-lazer/osu.AppImage>
- **Verification AppImage SHA-256:** `dced9463b501009c95dbed891abd2f0acc2efb84ee4336f5b1cc3b7c04a5fc7d`
- **Generated `assets/offsets.json` SHA-256:** `e4ee6be2e8880663eff36d2369af4b30411d87fe6a4711c9fbfe2cdef4f7de5a`
- **Generation runtime:** AppImage-bundled `Microsoft.NETCore.App 10.0.12`
- **Integrity manifest:** `assets/SHA256SUMS`

### Historical 2026.726.0 asset reproduction and .NET 8 bundle pins

The README's reproducible Satori/DAC recipe and the .NET 8.0.29 bundle package
hashes below document the prior `2026.726.0` asset-generation evidence only;
they are not provenance for the retained `2026.921.0` offsets. Cargo metadata
labels these values as historical. They remain useful to reproduce that earlier
asset, and do not imply that current offsets were generated with .NET 8 or Satori.

### Microsoft .NET runtime, host, and reference packs 8.0.29

- **Material:** self-contained .NET runtime and app-host embedded in the platform-matching generator bundle; end users do not install this runtime separately
- **Copyright:** .NET Foundation and Contributors
- **License:** MIT
- **Runtime NuGet package:** <https://www.nuget.org/packages/Microsoft.NETCore.App.Runtime.linux-x64/8.0.29>
- **Runtime NuGet archive SHA-256:** `780e71f4d31f0a0e8985198e28aa9f04bf3e4f0b505bfd4e36f25e00d3c93850`
- **Host NuGet package:** <https://www.nuget.org/packages/Microsoft.NETCore.App.Host.linux-x64/8.0.29>
- **Host NuGet archive SHA-256:** `4ff7979334f75774f561ab1cd5c519fe32ce589637ce9fc6c494c24f6e209c0c`
- **Reference NuGet package:** <https://www.nuget.org/packages/Microsoft.NETCore.App.Ref/8.0.29>
- **Reference NuGet archive SHA-256:** `77102e11453554e1254380a92fd1cb855e0a545b510c07194bc5b4193d838ba8`
- **Runtime source revision:** [`18fd75c847399745c43b5970fec840ba71064e80`](https://github.com/dotnet/runtime/tree/18fd75c847399745c43b5970fec840ba71064e80)
- **AppImage runtime-pack declaration:** `Microsoft.NETCore.App.Runtime.linux-x64/8.0.29`
- **AppImage declared `System.Private.CoreLib.dll` file version:** `8.0.2926.32403`
The embedded bundle carries the runtime and host license files from the restored
NuGet sources at `licenses/microsoft.netcore.app.runtime.<rid>/8.0.29/` and
`licenses/microsoft.netcore.app.host.<rid>/8.0.29/` (`LICENSE.TXT` and
`THIRD-PARTY-NOTICES.TXT`).

### ppy/Satori 2025.807.0 (historical asset evidence)

- **Material:** historical official osu! AppImage runtime-overlay and locally built DAC evidence retained for provenance; these files and the compatibility patch are not inputs to the embedded generator bundle and are not distributed by vosu
- **Copyright:** .NET Foundation and Contributors
- **License:** MIT
- **Release:** <https://github.com/ppy/Satori/releases/tag/2025.807.0>
- **Source revision:** [`098d0ed17cf8f965d79be6e3d870a895a6dbbe04`](https://github.com/ppy/Satori/tree/098d0ed17cf8f965d79be6e3d870a895a6dbbe04)
- **Linux x64 archive:** <https://github.com/ppy/Satori/releases/download/2025.807.0/linux-x64.zip>
- **Linux x64 archive SHA-256:** `aef780b177f77caf463d986cdb030366866bb3502039473dcc71dd2bdeb68fd2`
- **Runtime product version:** `8.0.16`
- **Release native version marker:** `42.42.42.42424`
- **Locally built DAC marker used by pinned ClrMD:** `0.0`, with the exact Satori source revision and compatibility-patch hash embedded alongside it
- **DAC compatibility patch:** `tools/lazer-offset-generator/patches/0001-guard-satori-gc-heap-data.patch`
- **DAC compatibility patch SHA-256:** `e0aaf89fff93b05dad0a4fc272683cbc852a3f51fd8d5b27ac8d91bafd3a597a`
- **`System.Private.CoreLib.dll` SHA-256:** `73c6ae7ff2ed535a249539f78731fc4412cf6c263ed9692fcf035f2cd01f96fa`
- **`libclrjit.so` SHA-256:** `3d58119a829d10d20f9fc31f2144f9202af19c93b412d5a5eeeeeba4b35ebb3e`
- **`libcoreclr.so` GNU Build ID:** `924f6642f039382780dd0ee782477271d8093084`
- **`libcoreclr.so` SHA-256:** `dcc631eda8fdb34d06956697e9709b8d828306eb73ac3187b5058180e1af615e`

### Microsoft.Diagnostics.Runtime (ClrMD) 4.0.732401

- **Material:** self-contained live-process inspection dependency embedded in the platform-matching generator bundle
- **Copyright:** .NET Foundation and Contributors
- **License:** MIT
- **NuGet package:** <https://www.nuget.org/packages/Microsoft.Diagnostics.Runtime/4.0.732401>
- **Pinned source:** <https://github.com/microsoft/clrmd/tree/f160f6897f33198170895a60444de38bcb76bcf3>

ClrMD and its restored build dependencies are not linked into vosu's Rust
daemon, but are embedded in the executable's generator payload. They are not
separate archive members: the two-entry production release archive still
contains only the executable and root `LICENSE`. The bundle carries
`LICENSE`, `LICENSE-MIT`, `LICENSE-APACHE`, `THIRD_PARTY_NOTICES.md`, and its
manifest for redistributed components.

## Maintained source-layout changes

The September 2026 cleanup separates dashboard settings into
`assets/server/homepage-settings.js` and settings/dialog/responsive styles into
`assets/server/homepage-controls.css`, loaded by the existing homepage. The
retained bytes are covered by `assets/SHA256SUMS`; `package.metadata.vosu` retains
the base stylesheet hash. These are local structural changes, not new upstream
revisions. Calculator attribution and unchanged retained test provenance are
recorded in `crates/vosu-pp/THIRD_PARTY_LICENSES.txt`.

The counter-management CSRF fix locally changes `homepage.js` management calls
and the report form in `homepage.html` to POST, with matching navigation styling
in `homepage.css`. These first-party changes do not change upstream provenance;
the exact retained bytes and base stylesheet metadata hash are updated together.

The October 2026 dashboard rework locally moves the page tabs and search into
the top bar, removes the overlay-tools links, `/local-overlays` guide, and
schema builder, renders installed overlays as equal-size cards (at most four
per row) with contained, enlargeable previews and a copy-URL bar, lists the
catalog one overlay per row with a browsable preview image, and makes
runtime settings a searchable, autosaving single column across `homepage.html`,
`homepage.js`, `homepage-settings.js`, `homepage.css`, and
`homepage-controls.css`. These first-party changes do not change upstream
provenance; the exact retained bytes and metadata hashes are updated together.

The offset tools' formerly tracked `bin/` and `obj/` files are reproducible local
build output, not distributed runtime inputs. Their source projects and package
locks remain; restoring/building follows the README's maintainer workflow.

## Direct Rust packages

The [dependency inventory](docs/dependency-license-inventory.json) records every locked third-party Cargo package resolved with `--all-features`, including build/development dependencies and platforms not shipped by Linux/Windows releases. It retains each version, archive checksum, declared license expression, source repository, and top-level packaged-notice hashes. Regenerate it with `python scripts/check-provenance.py --refresh-dependency-inventory` after manifest/lock changes. Input hashes normalize checkout CRLF to LF; upstream notice hashes preserve exact archive bytes. This inventory does not select license alternatives or certify complete redistribution notices: nested/vendored material, native/system libraries, and the embedded .NET payload require their own evidence. The direct-package summary below is not the exhaustive list.

### MIT

- `axum`, `bytes`, `dotenvy`, `open`, `rosu-map`, `rosu-mods`, `rosu-pp`, `rust-embed`, `serde-value`, `sysinfo`, `tokio`, `tokio-util`, `tracing`, `tracing-subscriber`, `winresource`, `zip`, and `zmij`
- local MIT forks `vosu-rosu-map` and `vosu-rosu-pp-api` (MIT; complete texts and pinned source links are in `crates/vosu-pp/THIRD_PARTY_LICENSES.txt`)

### MIT OR Apache-2.0 (or the equivalent reversed expression)

- `arc-swap`, `chrono`, `futures-util`, `indexmap`, `libc`, `md-5`, `reqwest`, `semver`, `serde`, `serde_json`, `sha2`, `tempfile`, `thiserror`, `url`, `windows-sys`, and Windows-only `wmi`

### Other declared expressions

- `memchr`: Unlicense OR MIT
- `notify`: CC0-1.0
- local project-authored code and `vosu-pp` package: `MIT OR Apache-2.0` where the project has licensing authority; separately identified retained inputs remain under their own terms
- Linux-only `libbpf-rs` and build-time `libbpf-cargo`: LGPL-2.1-only OR BSD-2-Clause. Their transitive `libbpf-sys` binding is BSD-2-Clause and statically includes libbpf 1.7.0 under LGPL-2.1-only OR BSD-2-Clause. Linux releases dynamically link the system libelf (LGPL-2.1-or-later OR LGPL-3.0-or-later) and zlib (Zlib) libraries.

### Development-only packages

- `criterion`: Apache-2.0 OR MIT
- `parking_lot`: MIT OR Apache-2.0

These development packages, the private `vosu-test-harness` workspace member, oracle tools, test fixtures, and benchmark code are not entries in the two-file production release archive.

## License text locations and sources

- project-authored repository code: `LICENSE-MIT` or `LICENSE-APACHE`
- release archive license member: `LICENSE` (combined first-party terms and retained calculator MIT texts)
  The `assets/SHA256SUMS` license entry tracks the current combined-license bytes (`8289c7cc59ca7272c0e0dd314288adacb7c2397dd2f5241e36457166c16f4719`). Refresh both values whenever `LICENSE` changes; other retained asset hashes remain tied to their exact bytes.
- calculator crate project-authored code: `crates/vosu-pp/LICENSE-MIT` or `crates/vosu-pp/LICENSE-APACHE`
- pinned lazer-calculator LGPL-3.0 reference: `tools/oracle/reference/lazer-calculator/LICENSE`
- local `vosu-rosu-pp-api` MIT license text: `crates/vosu-pp/rosu-pp-api/LICENSE`
- local `vosu-rosu-map` MIT license text: `crates/vosu-pp/rosu-map-api/LICENSE`
- calculator MIT texts and exact-source links: `crates/vosu-pp/THIRD_PARTY_LICENSES.txt`
- embedded asset integrity manifest: `assets/SHA256SUMS`
- Rust package source/checksum lock: `Cargo.lock`
- Rust package license metadata/source: the package manifests in the Cargo registry source selected by `Cargo.lock` and the repository links returned by `cargo metadata --locked --format-version 1`
