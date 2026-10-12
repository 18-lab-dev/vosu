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

## Relicensing permission (2026-10-12)

On 2026-10-12 the maintainer asked Mikhail Babynichev (KotRik), the copyright
holder named in tosu's `LICENSE`, whether the tosu-derived portions of vosu, the
Rust port of tosu code, could be distributed under MIT OR Apache-2.0 with
attribution to tosu and its contributors kept. He replied in writing: "No
problem, have fun! I give my permission". The maintainer retains the exchange.

- **Scope:** the tosu-derived Rust in vosu, including the memory readers, API
  projections, state, server, process, configuration, counter and updater code
  this audit traces to the July 2026 port. Those portions are distributed under
  the project's MIT OR Apache-2.0 terms with tosu credited in
  [`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md).
- **Upstream license:** he confirmed tosu is LGPL; the `GPL-3.0` in its
  `package.json` is an unmaintained npm field. That settles the discrepancy
  recorded below.
- **Other contributors:** he was asked to name any part he could not relicense
  himself and named none.
- **Not covered:** the former calculator wrapper and its retained reference
  `tools/oracle/reference/lazer-calculator` are LGPL work authored by
  storycraft. The `vosu-pp` adapter questions in the remediation review below
  are unchanged by this permission, and the retained reference keeps its own
  LGPL license.

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
`1a67fd64cc38fe60306328dde0a429e3211ca4ed`. The original audit did not locate
it. The 2026-10-09 history investigation recovered that exact object in the
local tosu fork: commits `fc36097a` and `1a67fd64`, both authored as `qt715`,
follow upstream `v4.25.0`. They add stable Random-mod seed reporting and remove
premature data-loop exits (21 inserted and two deleted lines). This identifies
the historical fork revision without substituting an upstream release or
rewriting the historical records.

Two separately identified upstream comparison archives were inspected:
[`v4.25.0 / 9f42c887`](https://github.com/tosuapp/tosu/tree/9f42c88714da69ac2ba248126a189416c55d733c)
and the current `2359305a` pin. Neither is identical to the historical fork revision.

## Production files requiring provenance review

This table is the dated 2026-10-02 comparison. The tosu-derived memory, API and
state rows are now covered by the
[relicensing permission](#relicensing-permission-2026-10-12); the dashboard row
is too, since that material came from tosu.

| Local files | Comparison and finding | Required follow-up |
| --- | --- | --- |
| `src/memory/stable/signatures.rs`, `settings.rs`, `readers.rs` | The audio/user/result readers have matching ordered pointer graphs and field offsets in tosu's `packages/tosu/src/memory/stable.ts`; the Rust bodies already exist in initial commit `7a49fda2` | The origin of the decoder implementation is unresolved, not merely the signature constants; require author/source evidence before assigning upstream-derived or independent status |
| `src/memory/lazer/*`, `src/api/v1.rs`, `src/api/v2/*`, `src/api/precise.rs`, `src/api/sc.rs`, `src/state/*` | The sampled StreamCompanion builder has the same status-dependent mode/mod selection and rate-scaled UR projection; lazer/state samples implement corresponding object/state contracts | This is a lightweight sample, not an exhaustive cross-language comparison. In particular, API-required mapping and common state concepts do not establish mechanical translation or independent implementation |
| `crates/vosu-pp/src/model.rs`, `data.rs` | Exact whitespace-normalized matches include `model.rs:169–194` versus former `model.rs:110–135` (26 lines), and `data.rs:5–22` versus former `data.rs:3–20` (18 lines) at `92b2f5e` | These include score-model fields and projection structures; matching API/data layout alone cannot establish origin or legal derivation |
| `crates/vosu-pp/src/performance.rs` | Current catch blocks match the former Rust; MIT osu!/rosu-pp supply the equation, and original authoring records confirm those source reads | The equation's MIT origin is substantiated. Signed-score adaptation and rewrite-source exposure are separate questions; see the remediation review below |
| `crates/vosu-pp/src/scoring.rs`, `gradual.rs`, `beatmap.rs`, `source_map.rs` | Permissive sources supply calculation behavior, while recovered rewrite records also show reads and relays from the former wrapper | Retain origin review for exposed adapters and projections. `SOURCE_PORTS` records behavior authorities, not implementation independence |
| `assets/server/homepage.js`, `homepage-settings.js`, `homepage.html`, `report.html`, `homepage.css`, `homepage-controls.css` | Against each tosu snapshot's nine HTML/JS/CSS/SCSS files, excluding Vue, none of the six assets as of 2026-10-09 shared a whitespace-normalized five-line block containing over 100 characters | The retired textual screen and the documented dashboard replacement are not proof of independent authorship; cross-language, non-contiguous and shorter transformations are outside this method |

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

The dashboard screen and former Rust wrapper block matches above came from
`scripts/audit-source-similarity.py`, retired after commit `d2c4380` and
recoverable from git history. It stripped whitespace per line but retained line
boundaries and all other tokens, indexed five-line windows for the dashboard,
and used `difflib.SequenceMatcher(..., autojunk=False)` for the wrapper,
keeping blocks of at least five lines and over 100 characters. It made no
authorship or licensing decision. The manual TypeScript/Rust comparisons above
were separate from this same-language textual screen. The findings remain
dated evidence; the screen is no longer maintained.

### Upstream license discrepancy and unresolved origin

Both inspected tosu revisions declare `GPL-3.0` in root `package.json`, but their
root `LICENSE` is LGPL v3 text with
`Copyright (C) 2023-2026 Mikhail Babynichev <https://kotrik.ru/>` substituted in
the license-document copyright line. Both retained source documents have
SHA-256 `cc1ea9718b312d17c286c953768912c0136299f5328b87c2d4be1fac6cc7c4b4`.
This is conflicting upstream evidence, not an unambiguous grant to choose or
broaden. Preserve the upstream attribution and evidence; seek clarification
before assigning terms to any confirmed tosu-derived production fragment.
**Resolved 2026-10-12:** tosu's copyright holder confirmed tosu is LGPL and
permitted MIT OR Apache-2.0 for the tosu-derived Rust; see
[Relicensing permission](#relicensing-permission-2026-10-12).

The listed memory/API/state/facade origins cannot be established confidently
from the available history. They remain explicitly flagged in this audit and
the PR, as issue #6 requires. No uncertain fragment is relabeled as MIT/Apache
or certified independent. If production derivation is established, preserve
the applicable upstream notice and license, identify the exact affected paths
and source revision, and resolve the combined-work distribution terms before
release. The existing historical LGPL reference and MIT forks retain their
own exact license files and licensing boundaries.

For the tosu-derived memory/API/state code, the
[2026-10-12 relicensing permission](#relicensing-permission-2026-10-12) settles
the distribution terms; the `vosu-pp` facade questions remain as recorded below.

## Remediation review (2026-10-09)

Reviewed tree: coordinator snapshot `9106c65e627479971d33fbcbeaf735ffb308407f`
(original HEAD `f075408209ee98cdd780f3a4d386d2a9850f40b0` plus existing
uncommitted work). The review re-checked the flags above against primary
sources. For `crates/vosu-pp` it asked what the current implementation is
actually supported by, rather than what historical labels say. Maintainer
decision recorded for this review: `vosu-pp` was intended as a rewrite that
references MIT osu!lazer and MIT rosu-pp, and relicensing vosu under GPL or LGPL
terms is not the chosen remedy. Nothing below is a legal clearance or a
clean-room certification.

### Method

The current Rust was compared with these pinned permissive authorities:

- ppy/osu `97e94babde9d5ecbe574ecdd33509db27452cd0f`
- ppy/osu-tools `d82cf2a986218f80c18dea5cc1399133d4a8586d`
- MaxOhn/rosu-pp `27a67242cad155a2767f5a25a423ca4ec3e3c274`
- MaxOhn/rosu-map `333dd2bea78e518d39c20932c828011f21d3305e`
- dotnet/runtime `15872212c29cecc8d82da4548c3060f2614665f7`

It was also compared with the retained LGPL reference,
`tools/oracle/reference/lazer-calculator` at `9a22c4c`, which is used here only
as historical evidence and a black-box oracle. Each match was classified as one
of four kinds:

- implementation expression;
- a formula or constant prescribed by osu!;
- a public compatibility field or observable behavior;
- boilerplate such as enums, signatures and error variants.

A match against vosu's own former Rust at `92b2f5e8` was not treated as proof of
an LGPL source. That Rust's provenance depends on where its expression came
from, which this section traces.

### Findings

- **What the historical records say.** Every `calculator-wrapper` item in
  `tools/oracle/calculator-diff-manifest.json` names a lazer-calculator file
  as its baseline location. The former crate notice at `92b2f5e8` declared
  `LGPL-3.0-only` because it "ports" the wrapper's PlayBeatmap, gradual patch,
  ScoreSimulator, AccuracyCalculator, hit-window, mod-conversion and
  data-conversion branches. These are prior claims about intent and
  comparison baselines, written while the former crate existed. They are not
  a source determination for the current Rust. The reference's
  `lib/package.json` (`LGPL-3.0-only`) and `LICENSE` (SHA-256
  `a5681bf9b05db14d86776930017c647ad9e6e56ff6bbcfdf21e5848288dfaf1b`) agree.
- **The wrapper's C# is thin.** Its 785 lines bind to osu!'s MIT calculators.
  - `ScoreSimulator.cs` and `AccuracyCalculator.cs` repeat MIT osu-tools
    `d82cf2a9` line for line. The only additions are namespaces, comments,
    signatures, dispatch, call arguments and the Classic accuracy gate at
    `AccuracyCalculator.cs:27,36,44`; the retired screen's osu-tools comparison reproduced this.
  - `HitWindows.cs` and `LazerMod.cs` forward to osu!'s hit-window and `APIMod`
    classes.
  - `Data/*.cs` define wire structs whose fields are named after osu!'s
    `HitResult` and attribute properties.
  - The gradual patch's new `GradualDifficultyEnumerator.cs` carries ppy's MIT
    header and reuses osu!'s `ProgressiveCalculationBeatmap`. Its authorship is
    storycraft's (all ten patches), and it ships inside the LGPL repository.
- **The catch formula traces to osu!.** The C# reference contains none of the
  catch constants (`0.0049`, `2500`, `0.475`, `0.97`, `1.35`, `5.5`).
  `performance.rs:206–283` follows MIT `CatchPerformanceCalculator.cs:34–103`
  and `ScoreInfoExtensions.cs:101–215` term by term, with C# unchecked `int`
  wrapping. `DiffUtils.Pow(0.97, int)` and `f64::powf` agree for exponents 0–5.
  Former Rust `engine.rs:544–586` at `92b2f5e8` is the same osu! translation.
  Its exact match with current `performance.rs:217–262` shows continuity of
  vosu's own Rust, not an LGPL source for the formula. Session history supplied
  by the coordinator's independent history review (not re-read by this
  reviewer) agrees. The original authoring session `019f5e79-3d6d` read the
  vendored MIT rosu-pp catch calculator and MIT osu!
  `CatchPerformanceCalculator.cs` between 2026-07-14 10:12 and 10:33, before
  writing the signed catch formula. This block is therefore recorded as a
  substantiated MIT-source translation, not an unresolved GPL/LGPL block, and
  is not rewritten.
- **`ScoreInfoData` is an interface-shape case, separate from calculation.**
  The same history review reports that the struct was authored at 09:26, after
  a 09:24:46 read of the wrapper's LGPL `Data/ScoreInfoData.cs`. Its field
  names are osu! `HitResult` names, and its order is the reverse of osu!'s
  `HitResult` enum, the same order the wrapper's struct uses. The struct carries
  no calculation; its serialized names and order are a consumer contract, so it
  is not changed. Whether adopting that interface arrangement from the wrapper
  needs permission is recorded below as an open compatibility-shape question.
- **The stable and API flags remain current** and are outside this
  assignment. All 16 `PATTERNS` and both chat/mania patterns in
  `src/memory/stable/signatures.rs` equal tosu `stable.ts:125–185` at
  `2359305a`. The reader correspondences now sit at `readers.rs:146–152`,
  `156` and `179`. `src/api/sc.rs:32–35,184–185` still mirrors
  `buildResultSC.ts`.

### Current-source determination for `crates/vosu-pp`

| Path | Classification | Pinned permissive source | Change made |
| --- | --- | --- | --- |
| `src/model.rs` | Public compatibility fields and boilerplate. `HitResult` values, `ScoreInfoData` counters (reverse `HitResult` order) and `LiveScoreState` legacy-count mapping | osu! `osu.Game/Rulesets/Scoring/HitResult.cs`, `osu.Game/Scoring/Legacy/ScoreInfoExtensions.cs` | None; field names and order are serialized contracts |
| `src/data.rs` | Projection of rosu-pp attribute and strain types into compatibility wire fields. The Taiko `effectiveMissCount` value and zero-filled fields are observable compatibility behavior | rosu-pp `src/any/{attributes,strains}.rs` and mode attribute types | `SOURCE_PORTS` records the observable behaviors as a black-box specification |
| `src/performance.rs` | rosu-pp dispatch plus the signed catch formula (prescribed by osu!) | rosu-pp `src/any/performance`; osu! `CatchPerformanceCalculator.cs:34–103`, `ScoreInfoExtensions.cs:101–215` | Attribution only |
| `src/scoring.rs` | Score generation and accuracy formulas; the Classic accuracy gate; total score | osu-tools `PerformanceCalculator/Simulate/*SimulateCommand.cs`, `PerformanceCalculatorGUI/RulesetHelper.cs`; rosu-pp `src/osu/score_state.rs:60–85,133–180` (`OsuScoreOrigin::Stable` omits slider tails and ticks); osu! `ScoreProcessor.cs:400–431` | osu-tools notice added; rosu-pp Classic authority recorded |
| `src/gradual.rs` | vosu facade over the rosu-pp gradual core: processed count, end-time skip, negative skip and rewind by rebuilding | rosu-pp `src/any/difficulty/gradual.rs` | Per-source stepping recorded as a compatibility specification |
| `src/beatmap.rs`, `src/beatmap/random.rs`, `src/mods.rs` | Translations of osu! mod transforms, rosu-mods decoding and a .NET seeded generator | osu! `ModDifficultyAdjust.cs`, `ManiaMod{Mirror,HoldOff,Invert,Random}.cs`, `TaikoMod{SimplifiedRhythm,Random}.cs`, `OsuModRandom.cs`, `OsuHitObjectGenerationUtils.cs`; rosu-mods 0.4.0; dotnet/runtime `Random.CompatImpl.cs:256–365` (also ported by rosu-pp `src/util/random/csharp.rs`) | **The missing dotnet/runtime MIT notice was added** |
| `src/hit_windows.rs` | Prescribed osu! window constants plus API shape | osu! `OsuHitWindows.cs:12–43`, `TaikoHitWindows.cs:12–37`, `ManiaHitWindows.cs:12–17,129–159`, `HitWindows.cs:21,111–117`, `CatchHitObject.cs:184` | Catch comment corrected (the empty windows come from `CatchHitObject`, not `DrawableCatchRuleset`); scope recorded as `Mixed` with the per-judgement Catch `None` contract |
| `src/difficulty.rs` | Glue over the rosu-pp gradual core | rosu-pp `src/any/difficulty` | None |
| `rosu-pp-api/src` | MIT fork forward-ported to osu!, plus two vosu compatibility additions: per-source gradual stepping in each mode's `difficulty/gradual.rs`, and a fixed 400 ms / 0.15 strain-graph projection in `osu/difficulty/skills/{aim,speed,reading}.rs` | rosu-pp `27a67242`; osu! `osu.Game/Rulesets/Difficulty/Skills/StrainSkill.cs:26–60,86–109` and `osu.Game.Rulesets.Osu/Difficulty/Skills/Flashlight.cs:31–33` | Additions disclosed in the notice; their observable values recorded as compatibility specifications |
| `rosu-map-api/src` | MIT fork with osu-framework-aligned geometry | rosu-map `333dd2be`; osu-framework `a0b031e7` | Notice wording only (no clean-room claim) |

**No calculation implementation was changed in this review.** The table
identifies permissive authorities for examined calculations; it is not a
blanket determination of implementation origin. The fork diff was inspected
for gradual and skill additions, not for every line of the 75-file forward
port.

Recovered authoring records contradict the August rewrite's clean-room claim.
The parent session `019ffb5e-c6e0` read old `data.rs`, `model.rs` and `engine.rs`
at 2026-08-13 13:51 before delegating the rewrite. It re-read old catch code
before writing the current signed block, and read the LGPL gradual patch.
`CleanScores` read the LGPL simulator/accuracy files; `CleanRoomOracle` read
the LGPL gradual and strain wrappers; the advisor relayed wrapper projections.
This establishes exposure, not a legal conclusion that every resulting
expression is protected or derived. Signed-score adapters, data/strain
projections, gradual boundaries, the Classic gate and compatibility-shape
arrangements remain under origin review. Their corresponding MIT formulas and
public contracts do not by themselves clear the implementation.

The original catch equation is a narrower finding: `LiveCalcOracle` read MIT
rosu-pp's catch calculator at 2026-07-14 10:12:13 and MIT osu!'s calculator at
10:12:17, before writing the formula at 10:36:21. That source tracing must not
be generalized into clearance of all surrounding wrapper code.

### Changes made by this review

- **Clean-room wording removed.** `crates/vosu-pp/THIRD_PARTY_LICENSES.txt`,
  `THIRD_PARTY_NOTICES.md`, both rosu-map fork notices and the embedded
  `LICENSE` appendix no longer state that the repository records clean-room
  work.
- **Missing MIT notices added.** The notices now include the ppy/osu-tools and
  dotnet/runtime MIT notices. `scripts/assemble-release-license.py` pins both
  retained texts, so the release `LICENSE` carries six MIT texts.
- **Historical labels described honestly.** The notices describe the wrapper
  labels as historical, keep the wrapper only as a parity reference and
  black-box oracle, and list the fork's compatibility additions.
- **`SOURCE_PORTS` corrected.** Entries now cite the supporting permissive
  paths: osu-tools, rosu-pp `OsuScoreOrigin::Stable`, osu! `StrainSkill` and
  `Flashlight`, mod and random sources, `EmptyHitWindows`, and dotnet/runtime.
  The four lazer-calculator-originated behavior sets are now explicit
  specification strings. `osu.signed-wrapper-results` and `wrapper.hit-windows`
  are `Mixed`. The public `SourceAuthority` enum is unchanged, and LSP found no
  use outside `source_map.rs`.
- **No calculation code, public API, serialized field, signed-count behavior
  or gradual semantics changed.**

### Disposition of each flag

| Flag | Disposition | Remaining question |
| --- | --- | --- |
| Stable `signatures.rs`, `settings.rs`, `readers.rs` | **Historical tosu port documented; relicensing permitted 2026-10-12.** Recovered `StableReader.jsonl` records a source-equivalent assignment and reads of `packages/tosu/src/memory/stable.ts` at 2026-07-14 04:02–04:03. Current correspondences were reproduced. tosu's copyright holder permitted MIT OR Apache-2.0 for this tosu-derived code | None for licensing; tosu attribution is retained |
| `src/memory/lazer/*`, `src/api/*`, `src/state/*` | **Historical translations reported by the authoring-session investigation; relicensing permitted 2026-10-12.** The investigation records tosu traversal and builder source use. tosu's copyright holder permitted MIT OR Apache-2.0 for this tosu-derived code | None for licensing; tosu attribution is retained |
| `vosu-pp` `model.rs`, `data.rs` | Permissive types support the field meanings and numerical attributes. The original struct followed a read of the LGPL `Data/ScoreInfoData.cs`; the rewrite consulted old projection code and wrapper-derived details | Data/strain projection origins and the compatibility-shape arrangement remain under review. Matching public fields do not alone establish protected derivation; cosmetic reordering is not a remedy |
| `vosu-pp` `performance.rs` | **Catch equation supported by MIT sources and original authoring records.** The current signed block was also written after reads of former Rust | Signed-score adaptation and surrounding implementation origins remain under review; the equation alone is not an external LGPL finding |
| `vosu-pp` `scoring.rs` | MIT osu-tools, rosu-pp and osu! support calculation behavior; rewrite records also show LGPL simulator/accuracy reads | Classic gating, signed-result adapters and implementation independence are not cleared by corresponding formulas |
| `vosu-pp` `gradual.rs`, `beatmap.rs`, `beatmap/random.rs`, `mods.rs`, `hit_windows.rs`, `difficulty.rs` and fork additions | Permissive sources support the examined algorithms; the rewrite also consulted prior fork code and the LGPL gradual/strain reference | Gradual boundaries and strain projections remain under origin review; the full fork delta was not independently re-audited |
| `vosu-pp` `source_map.rs` | **Corrected catalog**; a declaration, not implementation-history proof | None |
| Dashboard assets | **Not re-examined**; the 2026-10-02 screen stands with its stated limits | Maintainer evidence if a stronger conclusion is needed |

### Remaining uncertainty

**Current implementation origin.** The catch equation has substantiated MIT
source authority. The review does not establish independence of every exposed
adapter or fork addition, nor classify all matching contracts as protected
expression. The memory/API correspondences were outside this assignment.

**Historical and legal questions, not current-code findings:**

- Recovered transcripts identify agents that authored the original wrapper
  and expose the rewrite's source reads; edits outside those sessions remain
  unobserved.
- The license status of the gradual patch's MIT-headed files inside an LGPL
  repository.
- Whether observable compatibility parameters and the `ScoreInfoData`
  interface arrangement taken from the wrapper need permission.
- The historical pin is now identified as a local tosu fork revision; its
  upstream license discrepancy remains separate.
- The upstream tosu license conflict, resolved on 2026-10-12: tosu is LGPL.

No legal clearance is claimed.

### Source map and manifest consistency risks

- `tools/oracle/calculator-diff-manifest.json` is historical and unchanged. Its
  `authority` field (`osu-forward` or `calculator-wrapper`) records the
  original comparison baseline, not current sources, and is not mapped to
  `SourceAuthority`. The parity test checks only that scope names match in both
  directions and that both manifest authority values occur. It does not
  compare authorities, `upstream` or `specification` text.
- The osu-tools, dotnet/runtime and added osu! paths in `upstream` strings are
  not manifest pins; the manifest pins only osu!, the rosu-pp baseline and the
  wrapper.
- Manifest `rustLocation` values still name `src/lib.rs` targets from before
  the module split. They are historical and are not used to locate current
  code.

## Integrity and packaging

Since 2026-10-10 ([ADR 0012](decisions/0012-simplify-provenance-checks.md)),
`assets/SHA256SUMS` is verified by `scripts/check-asset-hashes.py`. The Cargo
metadata asset hashes, the notices digest cross-reference, the
dependency-license inventory, and `scripts/check-provenance.py` are retired;
bullets below that describe them record the state of this audit, not current
checks. The release `LICENSE` appendix check and packaging tests remain.

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
  six complete MIT texts for rosu-pp, rosu-map, osu!, osu-framework,
  osu-tools and dotnet/runtime (the last two added on 2026-10-09).
  `scripts/assemble-release-license.py` checks or refreshes that appendix from
  the maintained calculator notice. The two-entry archive shape is unchanged.
  This fixes the identified calculator-text omission, not every distribution
  obligation. The assembler checks each retained MIT text against its pinned
  content digest, normalizing only line endings and surrounding LF section
  separators; the original upstream-byte digests remain unchanged in notices.
- `docs/dependency-license-inventory.json` inventories all 362 locked
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
# Rebuild the retained calculator appendix in the release LICENSE, then
# refresh its digest in assets/SHA256SUMS.
python scripts/assemble-release-license.py --write
python scripts/check-asset-hashes.py --write

# Check the appendix, asset digests, and the release/development packaging scripts.
python scripts/assemble-release-license.py
python scripts/check-asset-hashes.py
python -m unittest discover -s scripts -p test_package_release.py
python scripts/check-doc-links.py
git diff --check

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

The retired source screen read the exact comparison revisions above, an extracted
`git archive 92b2f5e806fdc01e2edec1d6092f6812cb787521` tree, and an osu-tools
checkout. Its osu-tools comparison read the retained reference
`tools/oracle/reference/lazer-calculator` at lazer-calculator revision
`9a22c4c74f8e584fa64133a79a204dd045646bac` (`ScoreSimulator.cs` SHA-256
`c2d5b548a2c1d8ffd4bb36dfcb51e0fa3887a45875065d8bb8542e9e406ddeec`,
`AccuracyCalculator.cs` SHA-256
`cb50273c57ff9d703ec4617fe4c4da6dff692957ad99d97bceec31929d08f60e`; both match
the reference's `SHA256SUMS`), against ppy/osu-tools
`d82cf2a986218f80c18dea5cc1399133d4a8586d`. The 2026-10-09 run used these
upstream file digests:

| osu-tools file | SHA-256 |
| --- | --- |
| `PerformanceCalculator/Simulate/OsuSimulateCommand.cs` | `29d2b77c64b1a83bae00b3c5143923173d7d0e289d7cb2f5d6463fa9d6c3a054` |
| `PerformanceCalculator/Simulate/TaikoSimulateCommand.cs` | `06b5e199f04c9c944545cadc0d6aa715869aebd100a26ff14caad7d6e9b7c4c7` |
| `PerformanceCalculator/Simulate/CatchSimulateCommand.cs` | `2a6f4b75f3057e58a9f43a2500928d62ef5ba938022f98107ac92d8e37261307` |
| `PerformanceCalculator/Simulate/ManiaSimulateCommand.cs` | `f9d341e535bb207c1f2445b602d8ff9ca2b76cd5f3a75374fe0c8ffe5e0f00c2` |
| `PerformanceCalculatorGUI/RulesetHelper.cs` | `aeb1c797b3adf029cd533f42e622d4db42b37f15d21259d8819d554188444b3c` |
| `LICENCE` | `6e32fdd796385f89d57f0fbdee69178bc62deec64af0ce92011a21afb7a12191` |

It reported 36 residual `ScoreSimulator.cs` lines and 18 residual
`AccuracyCalculator.cs` lines, as characterized above.

The tagged release workflow builds each native target, packages and extracts
its actual binary, runs `vosu-release-smoke` against the extracted executable,
and checks the signed Windows resources. Run that workflow's package/extract,
`verify-set`, and smoke commands against real release builds before publishing.
The PR does not create a release or upload regenerated helper artifacts.

## Local remediation validation (2026-10-09)

The coordinator exercised the uncommitted notice/catalog patch against the
isolated snapshot `9106c65e627479971d33fbcbeaf735ffb308407f`; this is not a
published revision or attribution for inherited checkout changes.

- The new `--osu-tools` comparison ran against the pinned files above and the
  former Rust tree at `92b2f5e8`. Their hashes matched the recorded inputs.
  The reference residual counts were 36 and 18; the former-wrapper screen
  still reported matches in `model.rs`, `data.rs`, `performance.rs`, and
  `scoring.rs`. These observations do not clear any flagged origin.
- `assemble-release-license.py`, `check-provenance.py --package-fixtures`,
  all four `test_package_release.py` cases, maintained Markdown path checks,
  and all 18 asset checksums passed. Packaging fixtures are not executable
  release smoke tests.
- Rust 1.96.1 passed `vosu-pp` all-target/all-feature Clippy with warnings
  denied, the all-feature crate tests (72 tests), the focused `oracle_parity`
  gate (14 tests), and the changed `source_map.rs` formatting check.
- A throwaway consumer exercised the actual `LiveLazerEngine` API against one
  native map per ruleset: parse/convert, difficulty, score generation, accuracy,
  direct and retained-attribute performance, and gradual-prefix performance.
  All 20 structured outputs matched the pre-review capture exactly. This is
  compatibility smoke evidence, not an independent numerical or licensing oracle.
- Both helper bundles were rebuilt on Linux for `linux-x64` and `win-x64`.
  Each had 403 members, valid ZIP CRCs, the exact manifest/member hash set,
  and byte-identical current `LICENSE`, `LICENSE-MIT`, `LICENSE-APACHE`, and
  `THIRD_PARTY_NOTICES.md`. Building the Windows bundle here is not native
  Windows execution.

No new live osu! parity, native Windows runtime, signed-resource check, hosted
CI run, publication, or licensing clearance is claimed.

## Native validation record (2026-10-02)

The integrated implementation at `40cf2ada77438a4197597fc1b47891f1553d6943`
passed Rust 1.96.1 formatting, Linux workspace/all-target/all-feature Clippy,
the complete Linux CI test command, native Windows Clippy and the complete
native Windows CI test command, and both native release builds. The full
Windows cross-Clippy command also passed with `C_INCLUDE_PATH` unset.

Actual Linux and Windows tag and unsigned development archives passed
`vosu-release-smoke` against their extracted native executables. Windows
version fields, numeric version parts, language/codepage and native icon
checks passed using the resource-check portion of the production verifier.
The executable was `NotSigned`: Authenticode validation and the signed-release
publication gate were not exercised or bypassed.

Both offset-helper bundles were regenerated: each contained 402 members, with
valid ZIP CRCs, the exact manifest member set, matching member hashes and all
four attribution inputs. The declared-license inventory covered 362 locked
third-party packages. The 18 retained asset checksums, 24 historical calculator
checksums, canonical license copies, retained MIT appendix, maintained Markdown
links, packaging fixtures and four archive regression tests passed.

This record reports build/package behavior, not source independence, legal
clearance, live osu! parity or a published release. The production-origin
questions above remain explicitly recorded.


## Original validation record (2026-09-29)

Local validation passed the 18 asset hashes, canonical MIT/Apache checks,
first-party notice checks, exact calculator appendix check, maintained Markdown
links, ZIP regression tests, and both platform packaging fixtures. Pinned
Rust, PowerShell, and .NET were unavailable in the audit environment, so native
builds, native smoke tests, signed-resource inspection, and real helper-bundle
regeneration require CI or a suitably equipped host. Fixture archives are not
native build evidence.
