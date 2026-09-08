# Third-party notices

This file inventories third-party material retained by the vosu Rust daemon, which is derived from upstream tosu 4.25.0. It is an attribution aid, not a replacement for applicable license texts. Project-authored repository code and documentation are dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at the contributor's option; the release archive retains the root [`LICENSE`](LICENSE) member. Exact Rust package versions and checksums are authoritative in `Cargo.lock`.

Project copyright notice: Copyright © 2023–2026 Mikhail Babynichev. This notice applies to project-authored material only and does not alter separately licensed retained material.

## Native live-calculation sources

The production `vosu-pp` calculator is a clean-room Rust implementation. Its detailed source map and complete retained license texts are in [`crates/vosu-pp/THIRD_PARTY_LICENSES.txt`](crates/vosu-pp/THIRD_PARTY_LICENSES.txt).

### rosu-pp 4.0.1 baseline

- **Material:** public calculation APIs and ruleset semantics in the local clean-room source fork, plus the separate crates.io dependency used by the legacy API
- **Copyright:** Copyright (c) 2021 Max
- **License:** MIT
- **Pinned source:** <https://github.com/MaxOhn/rosu-pp/tree/27a67242cad155a2767f5a25a423ca4ec3e3c274>
- **License source:** <https://github.com/MaxOhn/rosu-pp/blob/27a67242cad155a2767f5a25a423ca4ec3e3c274/LICENSE>

### rosu-map 0.2.1

- **Material:** map parsing and slider-path geometry used by the local clean-room map fork
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

### ppy/osu-framework 2026.629.0

- **Material:** `PathApproximator`, `CircularArcProperties`, `Precision`, and vector-length semantics used to align slider geometry
- **Copyright:** Copyright (c) 2024 ppy Pty Ltd <contact@ppy.sh>
- **License:** MIT
- **Pinned source:** <https://github.com/ppy/osu-framework/tree/a0b031e7ff6c83d81da26fcbece7d25edbd222b4>
- **License source:** <https://github.com/ppy/osu-framework/blob/a0b031e7ff6c83d81da26fcbece7d25edbd222b4/LICENCE>
- **Verified license SHA-256:** `d07e3ce383a24bdc4665208e0ca3730e433ea1e8235b67a301a2b26335692c8e`

All four exact revisions and their MIT license files were verified from
fresh external checkouts. Production code depends only on the in-tree
clean-room Rust implementation and its MIT forks; the historical LGPL oracle
is not a build or runtime input.

### Historical calculator oracle boundary

`tools/oracle/reference/lazer-calculator` remains untouched as historical LGPL parity evidence at revision `9a22c4c74f8e584fa64133a79a204dd045646bac`. It is not a production source, crate dependency, build input, or runtime dependency. The current workspace and `vosu-pp` package are dual-licensed under `MIT OR Apache-2.0`; this does not relicense the retained historical reference or any other separately licensed material.

## osu!lazer memory-offset generation input

- **Material:** `assets/offsets.json`, generated locally with `tools/lazer-offset-target` and `tools/lazer-offset-generator` from the official osu! AppImage's managed assemblies and exact Satori runtime overlay plus a locally built matching DAC; the AppImage inputs, DAC, and published helper are not distributed by vosu
- **Copyright:** Copyright (c) ppy Pty Ltd <contact@ppy.sh>
- **License:** MIT for the osu! source code; osu! bundled resources remain subject to their upstream terms
- **osu! version:** `2026.726.0`
- **Release tag:** `2026.726.0-lazer`
- **Source revision:** [`5da71008b082d1a77e4bb301dc98886f1f24b895`](https://github.com/ppy/osu/tree/5da71008b082d1a77e4bb301dc98886f1f24b895)
- **Release:** <https://github.com/ppy/osu/releases/tag/2026.726.0-lazer>
- **Verification AppImage:** <https://github.com/ppy/osu/releases/download/2026.726.0-lazer/osu.AppImage>
- **Verification AppImage SHA-256:** `3d3028609543dffd037b00652603f75928515100b6245bc929a9dc46fd3b29a0`
- **Generated `assets/offsets.json` SHA-256:** `46b62194a297a9d9c1964837cc1f9d40aedfcd6c41673fc3d91a846aba7f20c0`
- **Integrity manifest:** `assets/SHA256SUMS`

### Microsoft .NET runtime, host, and reference packs 8.0.29

- **Material:** development-only self-contained base runtime, app-host, and reference packs for `tools/lazer-offset-target`; generation overlays the exact Satori runtime files shipped by osu! and does not distribute the published helper or runtime files
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

### ppy/Satori 2025.807.0

- **Material:** development-only `System.Private.CoreLib.dll`, `libclrjit.so`, and `libcoreclr.so` runtime overlay shipped by the official osu! AppImage, plus a locally built matching DAC with the committed GC-query compatibility patch used by `tools/lazer-offset-generator`; neither the overlay, DAC, nor published helper is distributed by vosu
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

- **Material:** development-only live-process inspection dependency of `tools/lazer-offset-generator`
- **Copyright:** .NET Foundation and Contributors
- **License:** MIT
- **NuGet package:** <https://www.nuget.org/packages/Microsoft.Diagnostics.Runtime/4.0.732401>
- **Pinned source:** <https://github.com/microsoft/clrmd/tree/f160f6897f33198170895a60444de38bcb76bcf3>
ClrMD and its restored build dependencies are used only by the maintainer generator. They are not linked into vosu and are not included in the two-file production release archive.

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

The offset tools' formerly tracked `bin/` and `obj/` files are reproducible local
build output, not distributed runtime inputs. Their source projects and package
locks remain; restoring/building follows the README's maintainer workflow.

## Direct Rust packages

The following license expressions come from the packages' Cargo metadata resolved by the committed `Cargo.lock`. Repository/source URLs and exact versions can be reproduced with `cargo metadata --locked --format-version 1`. Transitive crates are likewise fixed in `Cargo.lock`; their own packaged `license`/`license-file` metadata and registry source archives are the authoritative notices.

### MIT

- `axum`, `bytes`, `dotenvy`, `open`, `rosu-map`, `rosu-mods`, `rosu-pp`, `rust-embed`, `serde-value`, `sysinfo`, `tokio`, `tokio-util`, `tracing`, `tracing-subscriber`, `winresource`, `zip`, and `zmij`
- local clean-room `vosu-rosu-map` and `vosu-rosu-pp-api` (MIT; complete texts and pinned source links are in `crates/vosu-pp/THIRD_PARTY_LICENSES.txt`)

### MIT OR Apache-2.0 (or the equivalent reversed expression)

- `arc-swap`, `chrono`, `futures-util`, `indexmap`, `libc`, `md-5`, `reqwest`, `semver`, `serde`, `serde_json`, `sha2`, `tempfile`, `thiserror`, `url`, `wgpu`, `windows-sys`, and Windows-only `wmi`

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
- release archive license member: `LICENSE` (combined dual-license notice)
  The `assets/SHA256SUMS` license entry tracks these retained combined-license bytes (`f47dd0b3e8ddd61beb6123446a50ae0ad2a2bc1ad9da3c39f8a1b74525601c7b`); the 2026-09-05 audit corrected the stale checksum left by the earlier dual-license cutover without changing the license text.
- calculator crate project-authored code: `crates/vosu-pp/LICENSE-MIT` or `crates/vosu-pp/LICENSE-APACHE`
- pinned lazer-calculator LGPL-3.0 reference: `tools/oracle/reference/lazer-calculator/LICENSE`
- local `vosu-rosu-pp-api` MIT license text: `crates/vosu-pp/rosu-pp-api/LICENSE`
- local `vosu-rosu-map` MIT license text: `crates/vosu-pp/rosu-map-api/LICENSE`
- clean-room calculator MIT texts and exact-source links: `crates/vosu-pp/THIRD_PARTY_LICENSES.txt`
- embedded asset integrity manifest: `assets/SHA256SUMS`
- Rust package source/checksum lock: `Cargo.lock`
- Rust package license metadata/source: the package manifests in the Cargo registry source selected by `Cargo.lock` and the repository links returned by `cargo metadata --locked --format-version 1`
