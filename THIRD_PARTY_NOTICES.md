# Third-party notices

This file inventories third-party material retained by the vosu Rust daemon, which is derived from upstream tosu 4.25.0. It is an attribution aid, not a replacement for the applicable license texts. The root [`LICENSE`](LICENSE) is the authoritative LGPL-3.0-only license for vosu. Exact Rust package versions and checksums are authoritative in `Cargo.lock`.

Project copyright notice: Copyright © 2023–2026 Mikhail Babynichev. This notice is separate from, and does not modify, the canonical LGPL-3.0 license text.

## Native live-calculation sources

The detailed calculator notice is retained at `crates/vosu-lazer-pp/THIRD_PARTY_LICENSES.txt`; its source trees and pinned revisions are part of the reproducibility record.

### rosu-pp v4.0.1 baseline

- **Material:** `crates/vosu-lazer-pp/vendor/rosu-pp`
- **Copyright:** Copyright (c) Max Ohn
- **License:** MIT
- **License text:** `crates/vosu-lazer-pp/vendor/rosu-pp/LICENSE`
- **Source:** <https://github.com/MaxOhn/rosu-pp/tree/v4.0.1>
- **Recorded baseline:** commit `27a6724`, osu! baseline revision `28c846b4`

The separate legacy API also depends on crates.io `rosu-pp` 4.0.1, `rosu-map` 0.2.1, and `rosu-mods` 0.4.0, each under MIT according to its Cargo package metadata.

### osu! forward-port input

- **Material:** difficulty, performance, score-generation, structured-mod, and conversion changes forward-ported into the native calculator
- **Copyright:** Copyright (c) ppy Pty Ltd <contact@ppy.sh>
- **License:** MIT
- **Pinned source:** <https://github.com/ppy/osu/tree/97e94babde9d5ecbe574ecdd33509db27452cd0f>
- **License source:** <https://github.com/ppy/osu/blob/97e94babde9d5ecbe574ecdd33509db27452cd0f/LICENCE>

### Upstream tosuapp/lazer-calculator reference and wrapper-derived code

- **Material:** pinned development reference under `tools/oracle/reference/lazer-calculator`, including native wrapper sources and `lib/patches/0001-Gradual-diff-calculator.patch`; corresponding wrapper behavior is ported into `crates/vosu-lazer-pp`
- **Copyright:** Copyright (c) upstream tosuapp contributors
- **License:** GNU Lesser General Public License v3.0
- **Pinned revision:** `9a22c4c74f8e584fa64133a79a204dd045646bac`
- **License text:** `tools/oracle/reference/lazer-calculator/LICENSE`
- **Integrity/provenance:** `tools/oracle/reference/lazer-calculator/SOURCE_REVISION.json` and `SHA256SUMS`
- **Additional upstream credit:** the reference README credits `osu-tools` for score simulator code.

Because the Rust wrapper incorporates behavior derived from that LGPL-3.0-only source, the aggregate `vosu-lazer-pp` package declares `LGPL-3.0-only` in `crates/vosu-lazer-pp/Cargo.toml`. That package-level declaration does not relicense the separately identifiable MIT inputs: the unmodified baseline remains at `crates/vosu-lazer-pp/vendor/rosu-pp`, and the pinned osu! forward-port input remains the revision linked above. The crate package carries `crates/vosu-lazer-pp/LICENSE`; the production ZIP carries the authoritative root `LICENSE`; and the pinned wrapper reference retains its source license at `tools/oracle/reference/lazer-calculator/LICENSE`.

The reference tree and generator are development/oracle inputs. No C#, .NET binary, reference build output, or reference-source artifact is placed in the release ZIP.

## Direct Rust packages

The following license expressions come from the packages' Cargo metadata resolved by the committed `Cargo.lock`. Repository/source URLs and exact versions can be reproduced with `cargo metadata --locked --format-version 1`. Transitive crates are likewise fixed in `Cargo.lock`; their own packaged `license`/`license-file` metadata and registry source archives are the authoritative notices.

### MIT

- `axum`, `bytes`, `dotenvy`, `open`, `rosu-map`, `rosu-mods`, `rosu-pp`, `rust-embed`, `serde-value`, `sysinfo`, `tokio`, `tokio-util`, `tracing`, `tracing-subscriber`, `winresource`, and `zip`

### MIT OR Apache-2.0 (or the equivalent reversed expression)

- `arc-swap`, `chrono`, `futures-util`, `indexmap`, `libc`, `md-5`, `reqwest`, `semver`, `serde`, `serde_json`, `sha2`, `tempfile`, `thiserror`, `url`, `wgpu`, `windows-sys`, and Windows-only `wmi`

### Other declared expressions

- `memchr`: Unlicense OR MIT
- `notify`: CC0-1.0
- local `vosu-lazer-pp`: LGPL-3.0-only as an aggregate package; its separately identified vendored rosu-pp and osu!-derived inputs remain MIT

### Development-only packages

- `criterion`: Apache-2.0 OR MIT
- `parking_lot`: MIT OR Apache-2.0

These development packages, the private `vosu-test-harness` workspace member, oracle tools, test fixtures, and benchmark code are not entries in the two-file production release archive.

## License text locations and sources

- vosu LGPL-3.0-only: `LICENSE`
- aggregate `vosu-lazer-pp` LGPL-3.0-only package: `crates/vosu-lazer-pp/LICENSE`
- pinned lazer-calculator LGPL-3.0 reference: `tools/oracle/reference/lazer-calculator/LICENSE`
- vendored rosu-pp MIT text: `crates/vosu-lazer-pp/vendor/rosu-pp/LICENSE`
- calculator attribution summary: `crates/vosu-lazer-pp/THIRD_PARTY_LICENSES.txt`
- embedded asset integrity manifest: `assets/SHA256SUMS`
- Rust package source/checksum lock: `Cargo.lock`
- Rust package license metadata/source: the package manifests in the Cargo registry source selected by `Cargo.lock` and the repository links returned by `cargo metadata --locked --format-version 1`
