---
type: Contract
title: "vosu 2.0 ecosystem release matrix"
description: "Frozen F (ecosystem fan-out) identities, versions, settings and data transfer, pins, frozen 1.x catalog sources, platforms, per-operation publication owners and the receipt format."
---

# vosu 2.0 ecosystem release matrix

Status: frozen by the AF unit on 2026-11-24. This document is normative for
the ecosystem fan-out ([implementation plan](implementation-plan.md) section 7,
[ADR 0022](../decisions/0022-install-resolver-and-2-0-transition.md)). It
records decisions and pins; it is **not** evidence that any release, package,
crate or test exists or passed, and it authorizes no tag, push, upload,
signature, crates.io publication, visibility change or package-manager
publication. A value marked **pending** is filled in only by the named
owner operation or unit, in a later change to this file, with its receipt.

## 1. Decisions consumed

| Decision | Recorded by | Adopted content (summary; the linked text is normative) |
| --- | --- | --- |
| AF.1 release actions | Owner, 2026-11-24 | Every push or publication is approved separately, per operation, with exact version and target ([section 8](#8-publication-operations-and-owners)). `v2.0.0-alpha.1` is a GitHub prerelease with `make_latest=false`. |
| AF.2 identities and transfer | Main (delegated), 2026-11-24 | `*-v2` web ids, `vosu.vosulay`, `vosulay-editor`, same-kind `replaces` only, and one closed data/settings map executed once by M8 ([sections 3](#3-package-identities) and [5](#5-settings-and-durable-data-transfer)). |
| AF.3 long work | Main (delegated) | Core's 30-second invocation deadline is unchanged; long work is a plugin-owned operation behind short actions ([API v1](api-v1.md#plugin-owned-operations-af3)). |
| AF.4 coexistence | Owner | Each official repository keeps a `catalog.json` that points at frozen, independently packaged 1.x bytes ([section 2](#2-repositories-and-frozen-1x-sources)); support-window end decided later. |
| AF.5 visibility | Owner | vosu-tools, vosulay, vosu-overlays and the quickstart release publicly when F ships; the day-0 contract is not amended. |
| AF.6 uploads and exposure | Main (delegated) | Staged uploads, the exposure ledger and the demand-driven map-analysis timeline ([section 4](#4-actions-namespaces-and-exposure), [API v1](api-v1.md#staged-uploads-af6)). |
| AF.7 vosulay runtime | Owner | The supervisor discovers and validates a separately installed vosulay runtime; it never elevates or installs a setuid helper ([section 6](#6-pins-toolchains-and-platforms)). |
| AF.8 vosulay data authority | Main (delegated) | The attached child subscribes to API v1 itself; private IPC carries lifecycle, configuration and editor intent only ([D16 amendment](implementation-plan.md#10-adopted-decisions-and-risks)). |
| AF.9 SDK surface | Main (delegated) | Internal sync/bundled glue moves to a `publish = false` crate ([D11 amendment](implementation-plan.md#10-adopted-decisions-and-risks)). Implemented by F S0: `crates/vosu-plugin-internal`; `python scripts/sdk-package.py verify` checks the `.crate`. |
| AF.10 package checker | Main (delegated) | `vosu-package-check`, an SDK example over the SDK validator ([catalog v2](catalog-v2.md#package-checker-af10)). Implemented by F S0; registry-context format 1 is documented in the example's `context.rs`. |
| AF.11 live verification | Owner | Live runs use only the isolated VM test installs, under Main's VM ownership. |
| AF.12 artifacts | Main (delegated) | Invocation-owned protected artifacts ([plugin ABI §6.13](plugin-abi.md#613-invocation-owned-artifacts-af12), [API v1](api-v1.md#job-artifacts-af12)). |
| AF.13 private transports | Main (delegated) | Private renderer and CEF child transports are not tool services ([ADR 0019](../decisions/0019-plugin-runtime-and-trust.md#amendments), [plugin ABI §7.5](plugin-abi.md#75-bundled-only-http-surfaces-d3)). |

Inherited without reopening: D1, D7–D12, D16, D20 and D21; M6a Q1–Q7;
M7 A7.1–A7.5; M8 A8.1–A8.6, including the A8.6 supervisor smoke split; and
M5's adopted A5.1 option (b): no new daemon privilege, a plugin-host child
with zero permitted, effective, inheritable and ambient capabilities,
`no_new_privs` and seccomp denial of user-namespace creation, with CapBnd=0
no longer required and no CAP_SETPCAP.

## 2. Repositories and frozen 1.x sources

The official identities are exact strings; no alias, mirror or staging source
inherits their trust ([catalog v2](catalog-v2.md#trust)).

| Repository | Role | Frozen 1.x source (AF.4) | `catalog.json` SHA-256 | v1 entries | Visibility on 2026-11-24 |
| --- | --- | --- | --- | --- | --- |
| `18-lab-dev/vosu-tools` | official catalog source | `v0.4.3`, commit `8d3c4d45f533860ad81cd659a0bb9624c4e20dff`, published 2026-10-28 | `d14f8d2e54c4b59d0e44b844068eb211f38fa5ff70162a41eb1bce4be45cd5ab` | 8 tools | public |
| `18-lab-dev/vosulay` | official catalog source | `v1.5.3`, commit `a0eea2079f1e2e1fa7ec73c602f0e2d25769d373`, published 2026-11-08 | `e4c9dcd654bc280597778395215a54d76181193e00f7e12dc548a5c8d9bf1dd1` | 1 tool (`vosulay` 1.5.3; signed catalog and packages) | public |
| `18-lab-dev/vosu-overlays` | official catalog source | `v0.4.0`, commit `a85a9ffb93a97810fa4d848cee540814d74a4a77` (branch `feat/official-overlays`, not an ancestor of `main` `90e6371`), published 2026-11-16 | `78b585a101b8877849c0fcec1177163475a701cf9959be427bc49a687f1d26fd` | 19 overlays | public |
| `18-lab-dev/vosu-overlay-quickstart` | template; not a catalog source | none; no release published | none | none | **private** |
| `18-lab-dev/vosu` | host | latest stable `v1.2.6`; `v2.0.0-alpha.1` published 2026-11-25 as a signed GitHub prerelease (`make_latest=false`) from `v2/main` `025bf5f` | n/a | n/a | public |

The release rows were read on 2026-11-24 with read-only `gh release view`,
`gh repo view` and `gh release download -p catalog.json`; the hashes are of
the downloaded `catalog.json` bytes. All three catalog sources already have a
published 1.x release, so no new legacy coexistence release is needed. Each
v2 release's `catalog.json` is a byte-for-byte copy of the frozen asset above
unless the owner approves a later 1.x release before the first v2 release;
that release then replaces the row, by amendment, before any v2 catalog
copies it. Every URL in a frozen catalog is an immutable
`releases/download/<tag>/…` URL and stays reachable and licensed for the
support window. vosulay, which signs its 1.x catalog, publishes a valid
signature for the copied bytes under its own signing procedure. v2 sources
keep no 1.x service code, and no v1 metadata is generated around v2 bytes.

Each repository commits `tests/legacy-catalog.lock.json` ([section 9](#9-lock-files-and-receipts)) recording the row above
and the size and SHA-256 of every asset that catalog references.

**Overlay inventory (decided by the owner 2026-11-25).** The frozen `v0.4.0`
catalog lists 19 overlays: aim-scatter, catcher-heatmap, catcher-trail,
fruit-ledger, judgement-ratio, key-counter, key-overlay, kps-graph,
leaderboard, now-playing, now-playing-ticker, pp-counter, profile-card,
ratings-timeline (0.1.0), results-card, strain-graph, stream-interstitial,
taiko-drum and taiko-pace (each 0.4.0 except ratings-timeline). `main`
packages only ratings-timeline. The owner decided to **port all 18 others;
none is retired.** F's O0 unit ports each one from its `v0.4.0` source in
vosu-overlays to a `kind = "overlay"` v2 entry with a same-kind `replaces`
naming its 1.x id. Until its port is published, an installed one is left in
place, unmanaged by 2.0, and flagged only for the removed APIs it uses
([catalog v2](catalog-v2.md#first-start-migration-from-1x)). No v2 entry may
be invented for an overlay without its source.

## 3. Package identities

### Versions that move independently

| Axis | Value |
| --- | --- |
| Application (`host` ranges match this) | `2.0.0-alpha.N` prereleases, then `2.0.0`. The F landing head (f/alpha with M4a and M4b merged), from which `v2.0.0-alpha.1` is cut, declares `2.0.0-alpha.1`. |
| Alpha host range | `>=2.0.0-alpha.1, <3.0.0`, or a higher alpha minimum for a package that needs a later alpha's contract (B0 artifacts, P0 header asset). |
| Stable host range | `>=2.0.0, <3.0.0` unless the owner explicitly asks to keep an alpha-compatible range (A8.5). |
| SDK crate `vosu-plugin-api` | `1.0.0-alpha.N`, then `1.0.0`; major equals ABI major 1. `1.0.0-alpha.1` is the pre-B0 first publication (its approved `v2/main` commit predates the artifact codecs); the workspace now declares `1.0.0-alpha.2`, which adds B0's artifact opcodes and codecs. crates.io had no `vosu-plugin-api` crate on 2026-11-25. Each publication is a separate owner-approved operation (S1). Consumers pin it exactly (`=1.0.0-alpha.N`); a package using artifacts pins `=1.0.0-alpha.2` or later. |
| ABI / `manifest_version` / `catalog_version` | ABI 1; manifest format 1; catalog 2 for `catalog-v2.json`, 1 for the frozen `catalog.json`. |
| Core namespace schemas in `2.0.0-alpha.1` | `vosu.client`, `vosu.status`, `vosu.chart`, `vosu.play`, `vosu.skin` 1.0.0; `vosu.osu` 1.3.0, including M4a's `keys.overlay` physical keys. A package that requires `vosu.osu` 1.3.0 needs no host minimum above `2.0.0-alpha.1`. |
| Core action | `vosu.osu.beatmap.import` 1.0.0, `session = "required"`, `cli = true`. |
| New plugin namespaces, actions and schema assets | Each starts at `1.0.0` and moves only when its own contract changes; never bulk-rewritten with a package version. |
| Package `version` | Independent semver per entry, chosen by the component owner and recorded in the candidate receipt. |

### vosu-tools: seven plugins, seven web tools

Every plugin requests `runtime = "process"`, ships both native targets
(`x86_64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`), exports
`vosu_plugin_entry_v1` and depends only on the registry SDK. A change to
`runtime = "in-process"` for an official tool requires an amendment here.
Every web tool ships the single `any` package. Ids were checked against the
naming contract: each plugin id is three segments under the official-only
`vosu` prefix and neither extends nor is extended by any registered id
(the bundled `vosu.toolhost` is a different second segment).

| 1.x id (frozen version) | 2.0 plugin | 2.0 web tool | Web tool `replaces` |
| --- | --- | --- | --- |
| `lazer-library` (0.2.1) | `vosu.tools.lazer-library` | none; no UI is invented | none (cross-kind; [section 5](#5-settings-and-durable-data-transfer)) |
| `map-analysis` (0.3.0) | `vosu.tools.map-analysis` | `map-analysis-v2` | `["map-analysis"]` |
| `server-pp` (0.1.0) | `vosu.tools.server-pp` | `server-pp-v2` | `["server-pp"]` |
| `practice` (0.1.4) | `vosu.tools.practice` | `practice-v2` | `["practice"]` |
| `replay-renderer` (0.3.1) | `vosu.tools.replay-renderer` | `replay-renderer-v2` | `["replay-renderer"]` |
| `skin-studio` (0.5.0) | `vosu.tools.skin-studio` | `skin-studio-v2` | `["skin-studio"]` |
| `downloader` (0.1.1) | `vosu.tools.downloader` | `downloader-v2` | `["downloader"]` |
| `map-preview` (0.2.3) | none | `map-preview-v2` | `["map-preview"]` |

A plugin's `replaces` is empty: a plugin cannot replace a tool. Each web
tool requires its own plugin's `action:`/`field:` capabilities, so the
resolver installs the plugin as its dependency. map-preview requires only
core fields/resources and gains no action grant unless a concrete existing
mutation needs one.

### vosulay

| 1.x id (frozen version) | 2.0 plugin | 2.0 web tool | Web tool `replaces` |
| --- | --- | --- | --- |
| `vosulay` (1.5.3, native service tool) | `vosu.vosulay`, `runtime = "in-process"` (D16), both native targets | `vosulay-editor` (`any`) | `["vosulay"]` |

- `vosu.vosulay` is an external official plugin. It is never added to
  vosu's `BUNDLED` array and never ships inside the vosu executable.
- No id extending `vosu.vosulay` (for example `vosu.vosulay.supervisor`) may
  be registered beside it.
- The M7 A7.5 install target is the `kind = "tool"` entry `vosulay-editor`
  in `18-lab-dev/vosulay`'s `catalog-v2.json`. The editor requires the
  supervisor's actions, so the normal install plan adds `vosu.vosulay`.
  Installing the plugin does not install the vosulay runtime (AF.7).

### vosu-overlays and the quickstart

| 1.x id (frozen version) | 2.0 entry | `replaces` |
| --- | --- | --- |
| `ratings-timeline` (0.1.0) | overlay `ratings-timeline-v2` (`any`, root `manifest.toml` and `index.html`) | `["ratings-timeline"]` |
| 18 other `v0.4.0` overlays | one `kind = "overlay"` v2 entry each, ported by O0 from its `v0.4.0` source (owner decision 2026-11-25, [section 2](#2-repositories-and-frozen-1x-sources)); none is retired | same-kind, naming its 1.x id |
| sidecar pages `/lyrics`, `/map`, `/keys` | not packages; they stay a Python 3.12 sidecar in vosu-overlays | — |

The quickstart's `manifest.toml` is a `kind = "overlay"` template whose id
belongs to the user who copies it; no catalog lists it. Its latest release
tag must parse as semver major 2; the recommended first tag is `v2.0.0`.

## 4. Actions, namespaces and exposure

Bare action ids are `<plugin-id>.<name>` in calls and `action:<plugin-id>.<name>`
in manifests; each has exactly one `[[actions]]` entry, a bounded
args/result schema pair, a `session` mode and a `cli` flag. The per-route
ledger, with every action's schema asset path, session mode, CLI flag, state
pointer or resource kind, settings/data owner and named test, is T0's
`vosu-tools/docs/v2-migration.md`; for T6/T7 it covers every row of the F
plan's complex-router inventory. Renaming a frozen action below after T0
lands requires an amendment here.

| Plugin | Frozen action names |
| --- | --- |
| `vosu.tools.lazer-library` | `library.status`, `beatmaps.query`, `beatmapsets.query`, `skins.query`, `scores.query` |
| `vosu.tools.map-analysis` | `calculate`, `beatmap.query`, `timeline.query`, `sections.query` |
| `vosu.tools.server-pp` | `calculate`, `servers.query` |
| `vosu.tools.practice` | `generate.start`, `generate.status`, `generate.cancel`, `profiles.list`, `profiles.save`, `profiles.delete`, `generated.list`, `generated.delete`, `generated.open`, `import` |
| `vosu.tools.downloader` | `workspace.*`, `lookup`, `search.start`, `search.status`, `search.cancel`, `library.*`, `queue.*`, `download.status`, `download.pause`, `download.resume`, `download.retry`, `download.cancel`, `import` (the `*` sets are fixed by T0) |
| `vosu.tools.replay-renderer` | `context.query`, `replays.list`, `replay.upload.begin`/`append`/`finish`/`abort`, `replay.open`, `sizes.list`, `sizes.save`, `sizes.last.set`, `layout.set`, `export.settings.set`, `overlay.size.set`, `sizes.delete`, `overlays.list`, `assets.open`, `presets.list`, `presets.save`, `presets.delete`, `render.start`, `render.status`, `render.cancel`, `render.plan`, `render.output`, `preview.start`, `preview.state.set`, `preview.stop`, `skin.upload.begin`/`append`/`finish`/`abort`, `skins.delete` |
| `vosu.tools.skin-studio` | `library.query`, `skin.query`, `skin.save`, `folders.add`, `folders.forget`, `build.start`/`status`/`cancel`, `export.start`/`status`/`cancel`, `export.open`, `upload.begin`/`append`/`finish`/`abort`, `uploads.remove`, `plays.last`, `rows.query`, `derive`, `review`, `apply`, `restore` (each with `.start`/`.status`/`.cancel` forms where the work is long), `applied.query`, `applied.unfinished` |
| `vosu.vosulay` | `start`, `stop`, `restart`, `status` (all `session = "none"`), plus one concrete action per editor operation routed to the existing `ManageEditor` authority; never a `message`/passthrough action |

Session and CLI rules:

- Every feature that works today without a running game stays available
  without one: utility queries and edits are `session = "none"` (or
  `optional`), never `required`. Removing a no-game path is a regression,
  not a simplification.
- `vosu.osu.beatmap.import` calls from practice and downloader use the
  session handle captured when the user started the operation; focus changes
  never retarget them. Stable `queued`/F5-required and lazer `submitted` are
  reported as such, not as completed installation.
- Each existing `[cli]` command becomes `cli = true` actions with the same
  behavior; no plugin binary keeps an HTTP-serving subcommand.

Exposure ledger (AF.6). Public means readable under API v1 peer policy by
any overlay, tool or API client; protected means reachable only through an
action the caller holds a grant for.

| Plugin | Public (session namespace / session-published resources) | Protected (action results, B0 artifacts) |
| --- | --- | --- |
| lazer-library | `status` summary for the current session | query results; chart/skin/replay bytes for no-game use through B0 artifacts |
| map-analysis | `current`, `timeline`, `sections`, `status` (below) | `calculate`, `*.query` results |
| server-pp | `current`, `pp` | `calculate`, `servers.query` |
| practice | none required; T4 may publish current-chart views per session, never generated files or paths | profiles, generated list, previews (B0), import results |
| downloader | none required | workspace, lookup, search, library, queue and download state; credentials are never returned, even to its own UI |
| replay-renderer | none; its 1.x read list was empty | every former GET: context, replay list, replays, assets, presets, plans, outputs (B0) |
| skin-studio | only safe, session-published skin assets already public through `vosu.skin` | library, skin, exports (B0), plays, rows, applied state |
| vosulay | none | status and editor operations |

Raw host paths, credentials and private replay/skin lists never appear in a
public field, resource key, tree index, error message or log. Native path
access is a plugin-only `field:vosu.osu.internal.paths…` requirement naming
the exact stable/lazer subtree, never granted to a tool or overlay.

**Map-analysis timeline for data-only overlays (AF.6).** The overlay never
invokes `calculate`. `vosu.tools.map-analysis` publishes, per game session,
`/timeline/<system>` for each existing timeline system id (the current
`timeline_system` option set), computed only while a subscriber selects that
pointer (`DEMAND_CHANGED` selectors). Each subtree carries `generation`,
`checksum`, `rate`, `mode`, `system` and `status` (`pending`, `ready`,
`unsupported` or `error`) together with its bounded `points`, committed in one
`PUBLISH_BATCH`; a completion for a superseded generation is discarded, and
an unsupported mode is `unsupported`, never zero ratings. Each subtree fits
one 1 MiB frame and the namespace stays within the 4 MiB commit budget; T2's
namespace schema fixes the point bound. `current`, `sections` and `status`
follow the same generation rule.

## 5. Settings and durable data transfer

AF.2's closed map. M8's core migration executes it once, through its
journal (A8.3); plugin code never reads a 1.x tool root, and no plugin gets a
permission to copy arbitrary legacy directories. Secrets and settings the
native code consumes go to the plugin's `settings_schema`; presentation-only
settings go to the web tool's `settings.json`. A component unit may move a key
between the two columns only by amending this table.

| 1.x tool | Durable data `<data>/tool-data/<id>/` | To plugin settings | To web tool settings |
| --- | --- | --- | --- |
| lazer-library | → `vosu.tools.lazer-library` data dir | `lazer_data_dir` | — |
| map-analysis | → `vosu.tools.map-analysis` data dir | `local_window`, `local_hop`, `strain_window`, `strain_hop` | `map-analysis-v2`: `follow_game`, `show_etterna`, `show_prism`, `show_quaver`, `show_interlude`, `show_osu`, `show_reform`, `show_dan7k`, `show_danln`, `timeline_system` |
| server-pp | → `vosu.tools.server-pp` data dir | `calculators.folder`, `calculators.files`, `calculators.enabled`, `calculators.names`, `calculators.fuel`, `calculators.memory_mib` | — |
| practice | → `vosu.tools.practice` data dir | `lead_in_ms`, `lead_out_ms`, `gap_ms`, `fade_ms`, `pitch_with_rate`, `export_dir`, `open_exports` | `practice-v2`: `map_analysis_timeline` |
| replay-renderer | → `vosu.tools.replay-renderer` data dir | all 22: `audio.master`, `audio.master_mute`, `audio.music`, `audio.hitsounds`, `audio.mute`, `audio.fallback`, `audio.limiter`, `osu_dir`, `songs_dir`, `skin`, `mania.scroll_speed`, `resolution`, `fps`, `outro_ms`, `fade_ms`, `output_dir`, `encoder`, `export_for`, `discord_mb`, `parallel_jobs`, `render_gpu`, `ffmpeg_path` | — |
| skin-studio | → `vosu.tools.skin-studio` data dir | `stable_skins_dir`, `lazer_skins`, `credits` | `skin-studio-v2`: `start_ruleset` |
| downloader | → `vosu.tools.downloader` data dir | `concurrency`, `max_file_mb`, `no_video`, `songs_dir`, `sources`, `client_id`, `client_secret` (write-only secret) | `downloader-v2`: `hide_owned`, `follow_current_map` |
| map-preview | none (no service) | — | `map-preview-v2`: `skin`, `mania_scroll_speed`, `note_speed`, `show_bar`, `show_progress`, `controls_open` |
| vosulay | → `vosu.vosulay` data dir | the native keys V1 lists here before V2 lands (**pending V1**) | `vosulay-editor`: presentation keys V1 lists (**pending V1**) |

Rules:

- **Trigger.** A row runs after its target package's first successful
  install and before that package's first start. If the target is not yet
  installable (offline, unreachable or unpublished source), the row stays
  pending and the 1.x install stays in place, unmanaged.
- **Copy, never move.** The migration never deletes or rewrites 1.x bytes
  or 1.x configuration, so 1.x keeps working, as D21 already requires of
  alpha builds. Copies are durable before the row is marked done.
- **Collision.** A destination that already holds data, or a setting that
  already has a 2.0 value, is never overwritten or merged: the row records
  `collision`, leaves both sides untouched and the dashboard says so.
- **Restart.** An interrupted row resumes from its journal and is
  idempotent; a completed row never runs again.
- **Unlisted keys** are not migrated. A key a unit retires (for example a
  path the plugin no longer needs) is listed as retired here, never silently
  dropped.
- **Retirement of the 1.x record.** After a row completes, the 1.x tool
  record leaves catalog management. For lazer-library, which has no 2.0 UI,
  this happens when `vosu.tools.lazer-library` is installed through the map,
  not through `replaces`. The 1.x `replaces` move applies only to the web
  tool's presentation settings.
- **Secrets** are copied only into the plugin's write-only `password`
  settings, never into a web tool, a log or an action result.

## 6. Pins, toolchains and platforms

| Repository | Toolchain and runtime pins | Native CI and evidence |
| --- | --- | --- |
| vosu | Rust 1.96.1 (`rust-toolchain.toml`), `--locked` | `prepush.py --base v2/main`; Linux GNU and native Windows MSVC |
| vosu-tools | Rust 1.96.1 (`rust-toolchain.toml`, `rust-version`), Bun 1.4.3, Python 3.12 (CI) | `ubuntu-latest` and `windows-latest` CI matrix; X0 process receipts on both |
| vosulay | Rust 1.96.1 (`rust-toolchain.toml`), its own CEF pin and bootstrap, version line 1.5.x | `linux-ci.yml` (`ubuntu-26.04`); F adds own-CI Windows plugin jobs; A8.6 split |
| vosu-overlays | Python 3.12 (CI); sidecar CUDA path only on a configured sidecar host | CI adds tests, SDK package validation, browser/sidecar and X0 |
| quickstart | browser runtime and test dependencies committed by Q0 | new own CI (Q0) |

- **Host pin.** Each repository's `tests/vosu-alpha.lock.json` names one
  published host alpha and its per-target asset hashes. Before M8 the
  verifier checks for a newer adopted alpha, moves pins deliberately and
  reruns the affected native jobs; a receipt against a superseded alpha is
  stale. No unpinned "latest" binary is downloaded in CI.
- **SDK pin.** External plugins use the exact registry version and record
  the `.crate` SHA-256; no git, path or `[patch]` dependency on vosu, and
  no dependency on vosu-core, vosu-surface or private reader crates.
- **Supported targets.** `x86_64-unknown-linux-gnu` and
  `x86_64-pc-windows-msvc` for every native package; `any` for tools and
  overlays. A plugin missing either native target is not releasable.
- **Live verification (AF.11).** Windows stable and lazer use the agent VM
  test installs under `/home/user/gh/vecosystem/win-osu/`; Linux lazer uses
  the Linux gaming VM only. Main owns VM lifecycle; no personal install, host
  game session, broad process kill or VM restart.
- **Public support.** The only supported game is osu!; daemon platforms are
  Windows x64 and Linux GNU x64.
- **vosulay runtime prerequisite (AF.7).** The supervisor finds the vosulay
  runtime installed through vosulay's own channels (its release installer or
  archives, Winget, Scoop, AUR or distribution packages), validates the
  executable and version it will launch, and reports an actionable
  missing-runtime state otherwise. It never installs the runtime, never
  creates or modifies a setuid `chrome-sandbox`, never elevates, never runs
  CEF with `--no-sandbox`, and no catalog entry claims that unzipping a
  package provisions the sandbox. A bundled runtime would need a separately
  approved secure installer before catalog publication.

## 7. Version and tag lines

| Repository | Tag line | Day-0 requirement |
| --- | --- | --- |
| vosu | `v2.0.0-alpha.N` prereleases, `make_latest=false`; stable `v2.0.0` is an M8 owner release | — |
| vosu-tools | new v2 release line after `v0.4.3`; exact tags **pending** per-operation approval | latest, non-draft, non-prerelease release with `catalog-v2.json` and `catalog.json` |
| vosulay | its own approved line (currently 1.5.3); need not become 2.0.0 | same |
| vosu-overlays | new v2 release line after `v0.4.0`; exact tags **pending** | same |
| quickstart | `v2.x`, recommended `v2.0.0` | latest tag parses as semver major 2 |

Prereleases are integration artifacts and never satisfy GitHub
`/releases/latest`. A published tag is never moved; a defect needs a new
version. Release package URLs use `releases/download/<tag>/…`, never
`latest`.

## 8. Publication operations and owners

Every row is a separate owner approval naming the exact version, commit and
target; passing checks never grant one. **Main** performs approved
operations; unit agents only prepare them.

| Operation | Prepared by | Exact command and target (filled at approval) | Status |
| --- | --- | --- | --- |
| Land F vosu units onto `v2/main` | AF, R0, S0, B0, P0, D0 | `python scripts/prepush.py --base v2/main`, then `python scripts/land.py --base v2/main --topic <approved-topic>` at the exact HEAD | owner-blocked |
| Tag, draft, sign and publish `v2.0.0-alpha.1` as a prerelease, `make_latest=false` | R0 | release workflow for tag `v2.0.0-alpha.1` on the approved `v2/main` commit; `sign-release.py` (uploads signatures) | owner-blocked; R0 trigger |
| Publish `vosu-plugin-api` `1.0.0-alpha.N` to crates.io | S0 | `cargo publish --locked -p vosu-plugin-api` from the approved commit, after rechecking that the version is unoccupied | owner-blocked; S1 |
| Each later host alpha (B0 artifacts, P0 header asset) | R0/B0/P0 | as for alpha.1, new tag | owner-blocked |
| Each later SDK alpha (B0 codecs) | B0 | as for the first SDK alpha, new version | owner-blocked |
| Ecosystem prerelease tags and draft uploads (vosu-tools, vosulay, vosu-overlays, quickstart) | C0, V2, O0, Q0 | each repository's own release workflow at the approved tag | owner-blocked |
| Make `18-lab-dev/vosu-overlay-quickstart` public | Q0 | repository visibility change (AF.5 approves the outcome; the operation is still separately confirmed) | owner-blocked |
| Stable latest releases of vosu-tools, vosulay, vosu-overlays with both catalogs; quickstart `v2.0.0` latest | R1 | each repository's own release workflow; then unauthenticated `/releases/latest` readback | owner-blocked |
| Any later 1.x coexistence release | owner request only | that repository's 1.x release workflow; updates [section 2](#2-repositories-and-frozen-1x-sources) first | not planned |
| Winget, Scoop, AUR or other package-manager changes | vosulay and vosu package owners | outside F | not part of F |
| Live verification sessions | X0 | named VM install, temporary roots and owned process identities (AF.11) | approved in principle; each run scheduled by Main |

## 9. Lock files and receipts

### `tests/vosu-alpha.lock.json` (every ecosystem repository)

```json
{
  "lock_version": 1,
  "host": {
    "tag": "v2.0.0-alpha.N",
    "commit": "<40-hex>",
    "assets": { "<target>": { "url": "https://github.com/18-lab-dev/vosu/releases/download/<tag>/<file>", "sha256": "<64-hex>", "size": 0 } }
  },
  "sdk": { "crate": "vosu-plugin-api", "version": "1.0.0-alpha.N", "crate_sha256": "<64-hex>" },
  "schemas": { "vosu.osu": "1.3.0" }
}
```

`sdk` is omitted by repositories without Rust plugins. `schemas` lists every
foreign namespace revision the repository's packages require.

### `tests/legacy-catalog.lock.json` (each official catalog source)

```json
{
  "lock_version": 1,
  "repository": "18-lab-dev/<repo>",
  "tag": "<frozen 1.x tag>",
  "commit": "<40-hex>",
  "catalog": { "sha256": "<64-hex>", "size": 0 },
  "assets": [ { "url": "https://github.com/18-lab-dev/<repo>/releases/download/<tag>/<file>", "sha256": "<64-hex>", "size": 0 } ]
}
```

### Ecosystem receipt (one JSON file per run, job and target)

```json
{
  "receipt_version": 1,
  "kind": "own-ci-process | native-sandbox | browser | package | sdk-publication | release | live",
  "repository": "18-lab-dev/<repo>",
  "commit": "<40-hex>",
  "tag": "<tag> | null",
  "host": { "tag": "v2.0.0-alpha.N", "binary_sha256": "<64-hex>", "source": "github-release | local-build" },
  "sdk": { "version": "1.0.0-alpha.N", "crate_sha256": "<64-hex>" },
  "target": "x86_64-unknown-linux-gnu | x86_64-pc-windows-msvc | any",
  "runtime": "process | in-process | none",
  "child": "production | fixture:<fixture-id> | null",
  "packages": [ { "id": "", "kind": "plugin | tool | overlay", "version": "", "target": "", "file": "", "sha256": "", "size": 0 } ],
  "catalogs": [ { "file": "catalog-v2.json | catalog.json", "sha256": "", "size": 0 } ],
  "workflow": { "run_url": "", "run_id": 0, "job": "", "attempt": 1 },
  "tests": [ { "suite": "", "command": "", "selected": 1, "passed": 1, "failed": 0, "skipped": 0 } ],
  "native": { "sandbox": {}, "cleanup": [ { "pid": 0, "executable": "", "generation": 0, "outcome": "" } ] },
  "approvals": [ "<approval id: decision, date, approver>" ],
  "result": "pass | fail",
  "started_at": "<RFC 3339>",
  "finished_at": "<RFC 3339>"
}
```

A receipt is rejected when any of these holds:

- `selected` is zero, or a skipped test is counted as passed;
- the commit, host hash, SDK checksum, target or package hash differs from
  the candidate;
- the host alpha is older than the newest adopted alpha;
- a `fixture:` child is offered as production CEF evidence, or the reverse
  (A8.6);
- a process smoke is inferred from an asset's existence;
- `runtime` differs from what actually ran.

M8 E0/G0 consume these receipts inside its candidate evidence manifest
(A8.4); it may wrap them but does not reinterpret their fields.

## 10. Owner-blocked and pending items

- Every remaining operation in [section 8](#8-publication-operations-and-owners). The F
  trigger is met: `v2.0.0-alpha.1` was published on 2026-11-25. The crates.io
  publication of `vosu-plugin-api` `1.0.0-alpha.1` (S1) awaits its owner approval.
- Exact ecosystem tags for vosu-tools and vosu-overlays.
- Porting the 18 remaining `v0.4.0` overlays to v2 entries (O0; decided 2026-11-25, all ported, none retired; [section 2](#2-repositories-and-frozen-1x-sources)).
- vosulay settings rows (V1) and the vosulay runtime's minimum compatible
  version (V0).
- T0's action ledger, T2's timeline point bound, and the owner publication of
  the host alpha and SDK `1.0.0-alpha.2` carrying B0's artifact contract.
- The 1.x support-window end (AF.4).
