# Provenance audit for issue #6

Continuation audit: 2026-10-02, integrating PR head
[`608ca287`](https://github.com/shiruviaa/vosu/commit/608ca287f0e639cee49082a6e1f4a1a5a4638ef6)
with current main
[`d9bdfb4e`](https://github.com/shiruviaa/vosu/commit/d9bdfb4e90fa165c809bced20e699e5a71a82d0f).
The original 2026-09-29 audit used main
[`9c2187e6`](https://github.com/shiruviaa/vosu/commit/9c2187e683775485b8551fd6e7cb09b6e6786332),
retrieved 897 tracked files, and recorded tree
`3cc6e426a3d1cc368abbfbb3bda34f309cc92702`.

Issue #6 requests accurate attribution, preserved upstream terms, a lightweight
copy/translation screen, and explicit disclosure of files whose provenance
cannot be established confidently. It does not require a certification of a
clean-room development process. This audit records actual comparisons and their
limits; the first-party grant applies only where contributors have licensing
authority. Neither compatibility, a replacement commit, nor an excluded oracle
directory establishes source independence.

## Ownership decision

The initial snapshot, [`7a49fda2`](https://github.com/shiruviaa/vosu/commit/7a49fda2b1da0169ee59862a11310df69c3ab3ff),
already had the disputed project notice in `THIRD_PARTY_NOTICES.md`.
[`8d26b51b`](https://github.com/shiruviaa/vosu/commit/8d26b51b63880645e0c72398bdad17997b2f11d7)
introduced the MIT/Apache files and repeated that name six times across the
root and calculator `LICENSE`/`LICENSE-MIT` files. Neither event supplies an
assignment, an ownership declaration specific to vosu, or an independently
supported first-party notice. The repository namespace and Git account
metadata are not substitutes for that evidence.

On 2026-09-29, the maintainer selected `sxlvia` as the public attribution and
confirmed that vosu was started in 2026. This PR replaces all six disputed
first-party notices with `Copyright (c) 2026 sxlvia`. That confirmation supplies
the first-party attribution decision; it is not an assignment of third-party
rights or proof of independence of the files below. Windows `LegalCopyright`
uses the same notice, while `CompanyName` stays empty because no company has
been established. First-party User-Agent strings no longer name an upstream
contact as vosu's contact.

Both `LICENSE-APACHE` copies are identical canonical license text. Their
`Copyright [yyyy] [name of copyright owner]` line is an example in the appendix,
not a project attribution to fill in. Preserve both files and the matching
embedded Apache sections. Preserve all actual third-party notices and license
texts when replacing first-party notices.

## History and source boundaries

| Evidence | Supported conclusion | Remaining limit |
| --- | --- | --- |
| Initial snapshot `7a49fda2`; no parent commit | Imported dashboard material and an LGPL-declared calculator wrapper were present | The snapshot does not record how all Rust implementation files were created |
| Dashboard replacement `af20c120` (2026-07-26) | Vue, fonts, images, and old styles were removed; page and script sources were replaced | A replacement commit is not evidence of a separated clean-room process |
| Calculator replacement `4971b568` (2026-08-13) | The old wrapper crate was removed; the current facade and MIT forks were introduced | Common fragments require source tracing; moving a reference outside the build does not establish independence |
| Relicense commit `8d26b51b` (2026-08-13) | The repository began declaring `MIT OR Apache-2.0` for first-party material | The declaration is not evidence of authority over someone else's code |
| Cargo manifests, `build.rs`, and source include paths | The historical `tools/oracle/reference/lazer-calculator` directory is outside the production build graph | This checks build inputs, not the development process |

Current Cargo metadata and `tools/oracle/oracle-lock.json` use tosu
[`2359305a`](https://github.com/tosuapp/tosu/commit/2359305a63f2aaebb201bc8d2eee58177f52590e).
The GitHub commit API and exact-revision source archive resolved this pin.
Historical `tools/oracle/calculator-diff-manifest.json` and
`tools/oracle/calculator/binding.ts` still record
`1a67fd64cc38fe60306328dde0a429e3211ca4ed`. It is not a local Git object;
GitHub commit search returned zero results, and the original audit could not
resolve it in `tosuapp/tosu` or `shiruviaa/vosu`. No authenticated source mapping
was recovered. Those historical records are not rewritten or declared verified.

Two separately identified upstream comparison archives were inspected:
[`v4.25.0 / 9f42c887`](https://github.com/tosuapp/tosu/tree/9f42c88714da69ac2ba248126a189416c55d733c)
and the current `2359305a` pin. Neither replaces the unresolved historical pin.

## Production files requiring provenance review

| Local files | Comparison and finding | Required follow-up |
| --- | --- | --- |
| `src/memory/stable/signatures.rs`, `settings.rs`, `readers.rs` | The audio/user/result readers have matching ordered pointer graphs and field offsets in tosu's `packages/tosu/src/memory/stable.ts`; the Rust bodies already exist in initial commit `7a49fda2` | The origin of the decoder implementation is unresolved, not merely the signature constants; require author/source evidence before assigning upstream-derived or independent status |
| `src/memory/lazer/*`, `src/api/v1.rs`, `src/api/v2/*`, `src/api/precise.rs`, `src/api/sc.rs`, `src/state/*` | The sampled StreamCompanion builder has the same status-dependent mode/mod selection and rate-scaled UR projection; lazer/state samples implement corresponding object/state contracts | This is a lightweight sample, not an exhaustive cross-language comparison. In particular, API-required mapping and common state concepts do not establish mechanical translation or independent implementation |
| `crates/vosu-pp/src/model.rs`, `data.rs` | Exact whitespace-normalized matches include `model.rs:169–194` versus former `model.rs:110–135` (26 lines), and `data.rs:5–22` versus former `data.rs:3–20` (18 lines) at `92b2f5e` | These include score-model fields and projection structures; matching API/data layout alone cannot establish origin or legal derivation |
| `crates/vosu-pp/src/performance.rs` | Current `217–224` matches former `engine.rs:544–551` (8 lines); `245–262` matches `569–586` (18 lines). MIT osu!'s `CatchPerformanceCalculator.cs:32–104` independently supplies the base equation and length/miss/combo/AR/HD/FL/accuracy/NF factors | Trace formula authority separately from the signed score adaptation, wrapping arithmetic and dispatch. MIT authority for the equation does not establish the origin of the whole Rust function |
| `crates/vosu-pp/src/scoring.rs`, `gradual.rs`, `beatmap.rs`, `source_map.rs` | Source-scoring/progressive contracts overlap former wrapper responsibilities. The textual screen found small `scoring.rs` matches but no qualifying exact five-line `gradual.rs` match; rebuild-based rewind still has structural similarity | Simulation, signed results, prefix bookkeeping and rewind origins remain unresolved. `SOURCE_PORTS` is a declared behavior-authority catalog, not implementation-history proof |
| `assets/server/homepage.js`, `homepage-settings.js`, `homepage.html`, `report.html`, `homepage.css`, `homepage-controls.css` | Against each tosu snapshot's nine HTML/JS/CSS/SCSS files, excluding Vue, none of the six current assets shared a whitespace-normalized five-line block containing over 100 characters | A reproducible textual screen and the documented dashboard replacement are not proof of independent authorship; cross-language, non-contiguous and shorter transformations are outside this method |

### Concrete source comparisons

- Current `src/memory/stable/readers.rs:135–149` and the initial Rust reader
  use `ruleset + 0x44`, then `+0x10`, a length check against 40, and 40 reads
  with `LEADER_START + 4*i`. Upstream `stable.ts:222–252` at `9f42c887`
  and `:224–254` at `2359305a` have the corresponding ordered operations.
- Current `readers.rs:150–171` maps profile fields at `0x30`, `4`, `0xc`,
  `0x70`, `0x74`, `0x7c`, `0x80`, `0x84`, `0x9c`, `0x88`, `0x8c`, `0xac`;
  upstream `user()` begins at lines 254/256 in the two comparison revisions.
  Current `result_screen` starts at line 173 and upstream at 430/432, with
  result base `+0x38`, mod XOR offsets `+0xc/+8`, and matching score/date fields.
  These concrete correspondences warrant origin review; memory-layout
  requirements and added Rust safety handling do not settle the history.
- The matching former Rust calculator is
  [`92b2f5e8`](https://github.com/shiruviaa/vosu/commit/92b2f5e806fdc01e2edec1d6092f6812cb787521).
  Its declared LGPL boundary is comparison evidence, not permission to
  backfill MIT authority for a common block.
- The catch formula authority is MIT
  [`ppy/osu@97e94bab`, `CatchPerformanceCalculator.cs`](https://github.com/ppy/osu/blob/97e94babde9d5ecbe574ecdd33509db27452cd0f/osu.Game.Rulesets.Catch/Difficulty/CatchPerformanceCalculator.cs):
  lines 32–51 cover score extraction/base/length/miss, 53–90 combo/AR/HD/FL/
  accuracy, and 92–104 NF and hit-count helpers. The retained MIT rosu-pp fork
  separately implements ordinary catch performance. Formula attribution does
  not certify the signed wrapper's source independence.

`scripts/audit-source-similarity.py` reproduces the dashboard screen and former
Rust wrapper block matches, reports exact input-file hashes and line ranges,
and explicitly makes no authorship/licensing decision. Its method strips
whitespace per line but retains line boundaries and all other tokens. Dashboard
comparison indexes five-line windows; wrapper comparison uses
`difflib.SequenceMatcher(..., autojunk=False)` and keeps blocks of at least five
lines and over 100 characters. Manual TypeScript/Rust comparisons above are
separate from this same-language textual screen.

### Upstream license discrepancy and unresolved origin

Both inspected tosu revisions declare `GPL-3.0` in root `package.json`, but their
root `LICENSE` is LGPL v3 text with
`Copyright (C) 2023-2026 Mikhail Babynichev <https://kotrik.ru/>` substituted in
the license-document copyright line. Both retained source documents have
SHA-256 `cc1ea9718b312d17c286c953768912c0136299f5328b87c2d4be1fac6cc7c4b4`.
This is conflicting upstream evidence, not an unambiguous grant to choose or
broaden. Preserve the upstream attribution and evidence; seek clarification
before assigning terms to any confirmed tosu-derived production fragment.

The listed memory/API/state/facade origins cannot be established confidently
from the available history. They remain explicitly flagged in this audit and
the PR, as issue #6 requires. No uncertain fragment is relabeled as MIT/Apache
or certified independent. If production derivation is established, preserve
the applicable upstream notice and license, identify the exact affected paths
and source revision, and resolve the combined-work distribution terms before
release. The existing historical LGPL reference and MIT forks retain their
own exact license files and licensing boundaries.

## Integrity and packaging

- `assets/SHA256SUMS` has 18 entries. Only its `LICENSE` row changes for the
  first-party notice, scope clarification, and retained calculator notices. The digest repeated in
  `THIRD_PARTY_NOTICES.md` must match it.
- The integrated main branch's offsets are the `2026.921.0` asset. Its digest
  agrees with Cargo metadata and the manifest; the PR does not replace its bytes.
  Cargo metadata also credits the current tosu schema/template input. The
  attribution is preserved rather than silently treating all asset inputs as
  first-party or ppy-only.
- Tag release ZIPs contain exactly `vosu`/`vosu.exe` and `LICENSE`. Both the
  Python packager and Rust updater enforce that shape. Adding separate notice
  members without an updater change would make archives incompatible.
- `.gitattributes` preserves the exact license, notice, and asset bytes on
  Windows checkouts as well as Linux; automatic newline conversion must not
  invalidate their hashes or alter the retained license texts.
- The tag packager now checks ZIP CRCs and exact `LICENSE` bytes on creation,
  extraction, and release-set validation. This catches an old license inside
  an otherwise correctly shaped ZIP.
- The root `LICENSE` now embeds the retained calculator notices, including the
  four complete MIT texts for rosu-pp, rosu-map, osu!, and osu-framework.
  `scripts/assemble-release-license.py` checks or refreshes that appendix from
  the maintained calculator notice. The two-entry archive shape is unchanged.
  This fixes the identified calculator-text omission, not every distribution
  obligation. The assembler checks each retained MIT text against its pinned
  content digest, normalizing only line endings and surrounding LF section
  separators; the original upstream-byte digests remain unchanged in notices.
- `openwiki/dependency-license-inventory.json` inventories all 362 locked
  third-party Cargo packages, including build/dev/non-release-platform packages,
  with versions, archive checksums, SPDX declarations and top-level notice
  hashes. Twelve archives lack a top-level license/notice file under the
  inventoried filename prefixes: `anes`, `jni`, `jni-macros`, `jni-sys-macros`,
  `objc2-core-foundation`, `objc2-io-kit`, both `r-efi` versions,
  `rustls-platform-verifier-android`, `valuable`, and both Windows GNU `winapi`
  import-library packages. This does not mean they are unlicensed; it records
  missing packaged text instead of inventing copyright notices. Nested vendored
  files, license-alternative selection, system libraries and .NET dependencies
  remain separate from this declared-license inventory.
- The provenance check rejects an inventory that is stale for any recorded
  workspace manifest or lockfile, or does not cover each locked third-party
  identity/checksum exactly once. Input text hashes normalize CRLF to LF so
  Windows checkout policy does not invalidate the same Cargo inputs.
- `scripts/version-build.py` includes the audit and dependency inventory in its
  15 attribution/provenance inputs, preserving the updated evidence in unsigned
  development archives. Their content is checked against the source checkout.
- `scripts/prepare-offset-generator.py` fingerprints the license/notices
  inputs and regenerates `bundle.json`, archive SHA-256, and its input marker.
  Rebuild affected generator bundles instead of editing generated hashes.

The cleanup resolves issue #6's first-party-attribution and wording requirements.
Its requested lightweight audit is complete as a bounded screen, with the
unresolved production paths, historical pin and upstream-license discrepancy
explicitly disclosed. Closing that cleanup issue does not certify clean-room
history, legal ownership of every fragment, or unrestricted release compliance.
Those questions must not be hidden by the updated first-party notice.

## Refresh and validation commands

From the repository root, after an approved notice or retained-text change:

```sh
# Rebuild only the retained calculator appendix, then refresh the LICENSE row
# and its repeated notice digest. Other asset hashes must already match.
python scripts/assemble-release-license.py --write
python scripts/check-provenance.py --refresh-license-hash --refresh-dependency-inventory

# Regenerate the full declared Cargo license inventory after manifest/lock changes.
python scripts/check-provenance.py --refresh-dependency-inventory

# Check retained text, all asset hashes, first-party copies, Windows metadata
# expectations, and Linux/Windows tag and development packaging with fixtures.
python scripts/assemble-release-license.py
python scripts/check-provenance.py --package-fixtures
python -m unittest discover -s scripts -p test_package_release.py
python scripts/check-doc-links.py
git diff --check
sha256sum --check assets/SHA256SUMS

# Regenerate offset-helper bundles from the changed license/notices inputs on
# a host with the pinned .NET prerequisites. The script writes bundle.json,
# the archive checksum, and the input fingerprint; do not edit those by hand.
python scripts/prepare-offset-generator.py --rid linux-x64
python scripts/prepare-offset-generator.py --rid win-x64

# Pinned Rust 1.96.1; run the native OS checks described in AGENTS.md as well.
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --lib --bins --tests --examples --all-features --locked
cargo test --locked --test updater_compat
cargo test --locked --test updater_native -- --nocapture
```

For the source screen, acquire the exact comparison revisions above and extract
`git archive 92b2f5e806fdc01e2edec1d6092f6812cb787521` into a separate directory:

```sh
python scripts/audit-source-similarity.py \
  --tosu <tosu-9f42c887-checkout> --tosu <tosu-2359305a-checkout> \
  --former-wrapper <vosu-92b2f5e-tree>
```


The tagged release workflow builds each native target, packages and extracts
its actual binary, runs `vosu-release-smoke` against the extracted executable,
and checks the signed Windows resources. Run that workflow's package/extract,
`verify-set`, and smoke commands against real release builds before publishing.
The PR does not create a release or upload regenerated helper artifacts.

## Original validation record (2026-09-29)

Local validation passed the 18 asset hashes, canonical MIT/Apache checks,
first-party notice checks, exact calculator appendix check, maintained Markdown
links, ZIP regression tests, and both platform packaging fixtures. Pinned
Rust, PowerShell, and .NET were unavailable in the audit environment, so native
builds, native smoke tests, signed-resource inspection, and real helper-bundle
regeneration require CI or a suitably equipped host. Fixture archives are not
native build evidence.
