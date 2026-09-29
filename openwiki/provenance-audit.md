# Provenance audit for issue #6

Audit date: 2026-09-29. Baseline: default branch `main`, commit
[`9c2187e683775485b8551fd6e7cb09b6e6786332`](https://github.com/shiruviaa/vosu/commit/9c2187e683775485b8551fd6e7cb09b6e6786332).
All 897 tracked files were retrieved and checked against their Git blob IDs;
the reconstructed tree was `3cc6e426a3d1cc368abbfbb3bda34f309cc92702`.

This audit supports a wording and attribution cleanup. The maintainer has
confirmed the public first-party notice as `Copyright (c) 2026 sxlvia` and
that vosu was started in 2026. Source independence is not certified; the
remaining questions stay open in [issue #6](https://github.com/shiruviaa/vosu/issues/6).

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

The recorded tosu revision `1a67fd64cc38fe60306328dde0a429e3211ca4ed`
could not be resolved through GitHub in either `tosuapp/tosu` or
`shiruviaa/vosu`. It remains unchanged in Cargo metadata, oracle locks,
manifests, and fixtures. Recover the exact source snapshot or an authenticated
mapping before claiming that pin is verified. Upstream tag `v4.25.0` currently
resolves to [`9f42c88714da69ac2ba248126a189416c55d733c`](https://github.com/tosuapp/tosu/commit/9f42c88714da69ac2ba248126a189416c55d733c);
that revision was used only as a separately identified comparison source.

## Production files requiring provenance review

| Local files | Comparison and finding | Required follow-up |
| --- | --- | --- |
| `src/memory/stable/signatures.rs`, `src/memory/stable/settings.rs`, `src/memory/stable/readers.rs` | Stable signatures and corresponding address names match `packages/tosu/src/memory/stable.ts` at upstream `v4.25.0` | Establish the origin of the constants, table selection, and associated decoder logic; do not infer licensing from identical compatibility data alone |
| `src/memory/lazer/*`, `src/api/v1.rs`, `src/api/v2/*`, `src/state/*` | Initial implementation arrived in a single snapshot; sampled code implements the same memory and projection contracts as upstream | Recover implementation notes or perform source-by-source review for mechanical translation; this audit did not establish a specific copied algorithm here |
| `crates/vosu-pp/src/model.rs`, `crates/vosu-pp/src/data.rs` | Whitespace-normalized blocks of 26 and 18 lines respectively also occur in the former `vosu-lazer-pp` crate at `92b2f5e` | Separate API-required structures from copied expression, and identify each fragment's MIT or independently specified authority |
| `crates/vosu-pp/src/performance.rs` | Catch-performance formula blocks of 8 and 18 lines also occur in the former wrapper's `src/engine.rs` | Trace these to the declared MIT inputs; common mathematics does not by itself prove LGPL derivation or independence |
| `crates/vosu-pp/src/scoring.rs`, `src/gradual.rs`, `src/beatmap.rs`, `src/source_map.rs` within that crate | The source map declares MIT, independent-specification, or mixed authorities; black-box vectors are named without implementation-process evidence | Verify the claimed authority for wrapper-specific behavior, including signed results, score simulation, and rewind/prefix behavior |
| `assets/server/homepage.js`, `homepage-settings.js`, `homepage.html`, `report.html`, `homepage.css`, `homepage-controls.css` | No whitespace-normalized common block of at least five lines and over 100 non-whitespace characters was found against upstream `v4.25.0` HTML/JS/CSS/SCSS, excluding Vue | This is a limited similarity check, not proof of independent authorship; retain the dashboard replacement history |

If a reviewed production fragment is confirmed to derive from LGPL-covered
source, retain its upstream copyright and applicable license, identify the
exact paths and revisions in the notices, and resolve the distribution terms
before release. This patch does not assign a new SPDX identifier to any
production source or change any retained upstream license.

## Integrity and packaging

- `assets/SHA256SUMS` has 18 entries. Only its `LICENSE` row changes for the
  first-party notice, scope clarification, and retained calculator notices. The digest repeated in
  `THIRD_PARTY_NOTICES.md` must match it.
- The offset notice had a stale digest. Its corrected current-byte digest now
  agrees with the unchanged asset and `package.metadata.vosu.lazer-offsets-sha256`.
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
  A complete inventory of other direct/transitive dependency notices remains
  outstanding; these checks do not certify distribution compliance.
- `scripts/version-build.py` already copies 13 attribution/provenance files
  into unsigned development archives. Their content must match the checkout;
  that inventory is not a complete audit of all transitive dependency notices.
- `scripts/prepare-offset-generator.py` fingerprints the license/notices
  inputs and regenerates `bundle.json`, archive SHA-256, and its input marker.
  Rebuild affected generator bundles instead of editing generated hashes.

The first-party name/year decision and the known calculator notice omission
are resolved by this PR. Keep issue #6 open for the unresolved source pin,
the production fragments above, and the remaining dependency notice inventory.

## Refresh and validation commands

From the repository root, after an approved notice or retained-text change:

```sh
# Rebuild only the retained calculator appendix, then refresh the LICENSE row
# and its repeated notice digest. Other asset hashes must already match.
python scripts/assemble-release-license.py --write
python scripts/check-provenance.py --refresh-license-hash

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

The tagged release workflow builds each native target, packages and extracts
its actual binary, runs `vosu-release-smoke` against the extracted executable,
and checks the signed Windows resources. Run that workflow's package/extract,
`verify-set`, and smoke commands against real release builds before publishing.
The PR does not create a release or upload regenerated helper artifacts.

Local validation passed the 18 asset hashes, canonical MIT/Apache checks,
first-party notice checks, exact calculator appendix check, maintained Markdown
links, ZIP regression tests, and both platform packaging fixtures. Pinned
Rust, PowerShell, and .NET were unavailable in the audit environment, so native
builds, native smoke tests, signed-resource inspection, and real helper-bundle
regeneration require CI or a suitably equipped host. Fixture archives are not
native build evidence.
