# vosu 2.0 state schema

This document specifies the 2.0 state model decided by [ADR 0020](../decisions/0020-game-agnostic-data-model.md)
and the resource reference shape decided by [ADR 0021](../decisions/0021-api-v1-and-upstream-adapters.md).
It is binding for implementers of core, the official osu! plugin, and any
third-party plugin or overlay that reads state. The phase 0 shared naming
contract governs plugin ids, capability ids, and session semantics used
throughout; this document does not repeat it.

JSON Schemas for every namespace live in `docs/v2/schemas/`, one file per
namespace plus `common-defs.schema.json` for shared `$defs` the others `$ref`
by relative path (`common-defs.schema.json#/$defs/<name>`). Loading one schema
file in isolation is valid JSON and a valid JSON Schema (draft 2020-12); fully
resolving `$ref`s needs the whole directory, as with any multi-file schema.

Capability ids for state are `field:<namespace>.<path>` (a leaf) or
`field:<namespace>` (the whole namespace), per the naming contract. The
manifest shapes that declare `provides`/`requires`/`optional` with these ids
are specified in `docs/v2/catalog-v2.md` (CatalogV2Spec); this document only
defines what the ids address.

## Namespaces

| Namespace | Schema | Owner | Filled by game plugins |
| --- | --- | --- | --- |
| `vosu.client` | `schemas/vosu.client.schema.json` | core | required |
| `vosu.status` | `schemas/vosu.status.schema.json` | core | best effort, declared |
| `vosu.chart` | `schemas/vosu.chart.schema.json` | core | best effort, declared |
| `vosu.play` | `schemas/vosu.play.schema.json` | core | best effort, declared |
| `vosu.skin` | `schemas/vosu.skin.schema.json` | core | best effort, declared |
| `vosu.osu` | `schemas/vosu.osu.schema.json` | the official osu! plugin | that plugin's contract |

A session's full state is the union of every namespace a plugin publishes for
it, keyed by namespace id at the top level:

```json
{
  "vosu.client": { "...": "..." },
  "vosu.status": { "...": "..." },
  "vosu.chart": { "...": "..." },
  "vosu.play": { "...": "..." },
  "vosu.skin": { "...": "..." },
  "vosu.osu": { "...": "..." }
}
```

`vosu.client` is always present for an attached session. The common
namespaces (`vosu.status`, `vosu.chart`, `vosu.play`, `vosu.skin`) are present
whenever the providing game plugin declares any coverage for them at all; a
namespace the plugin's manifest declares no fields for is omitted entirely,
never published as an all-null object. Within a declared namespace, each
field the plugin does not individually fill is `null` (ADR 0020): never a zero,
empty string, or empty array standing in for "unknown".

## `vosu.client`

Required. Identifies the attached client exactly as the shared naming
contract defines its fields (`game`, `variant`, `name`, `version`, `pid`,
`session`, `focused`, `plugin.{id,version}`); see `schemas/vosu.client.schema.json`
for types and `pid`/`session` constraints. Core fills this namespace from the
plugin's attach handshake; plugins do not publish it directly.

## Common layer: `vosu.status`, `vosu.chart`, `vosu.play`, `vosu.skin`

These hold values most rhythm games share (ADR 0020). Filling them is best
effort but declared: a game plugin's manifest lists exactly which fields of
these namespaces it fills, and the dashboard and resolver use that declaration
rather than probing live state.

### `vosu.status`

One field, `status`, an enum of `menu`, `selection`, `play`, `pause`,
`results`, or `null`. Core does not infer status from other fields; a plugin
whose client has a state with no exact match uses the nearest listed value.

### `vosu.chart`

Chart (beatmap, in osu! terms) metadata: `title`, `artist`, `difficultyName`,
`charter`, `length` (ms, unaffected by rate), `bpm` (a representative tempo,
unaffected by rate), `keyCount` (null for charts/rulesets without a fixed lane
count), and `resources` — an object of [resource references](#resource-references)
keyed by kind. Declared common kinds are `chart.file`, `chart.audio`,
`chart.background` (resource references) and `chart.set` (a tree reference);
plugins may add further kinds in their own namespace (`<plugin-id>.<kind>`),
also inside this object, per ADR 0021. A kind the current chart has no file
for is omitted from `resources`, never present with a reference to nothing.

### `vosu.play`

Universal play values: `score`, `combo`, `maxCombo`, `accuracy` (0-1, under
the formula named by `accuracyKind`), `accuracyKind` (never normalized across
games), `health` (0-1, null for resource-less rulesets), `progress` (0-1,
chart time elapsed over chart length), `rate` (effective playback rate
multiplier), and `judgements`.

`judgements` is the game-declared breakdown (ADR 0020): an array of
`{id, displayName, ordinal, count}`, ordered best to miss by `ordinal` (`0` is
best). Core never maps one game's judgement ids onto another's, and an
overlay reading `judgements` generically groups or sums by `ordinal` rather
than assuming any `id` is universal.

### `vosu.skin`

The client's current skin: `name` and `tree` (a [tree reference](#resource-references)
to its files, or `null` when the plugin does not fill this field or the
active skin has no browsable files).

## `vosu.osu`

The official osu! plugin's namespace: every datum the 1.x `/json/v2` and
`/json/v2/precise` documents exposed, restructured for clarity rather than
copied in their wire shape (the 1.x wire shape itself stays exactly as-is
inside the upstream adapter, ADR 0021). `schemas/vosu.osu.schema.json` is
authoritative for types; this section explains the restructuring and gives the
full 1.x-to-2.0 mapping.

### Restructuring, at a glance

- `vosu.osu.session` holds osu!-specific session bookkeeping (`playTime`,
  `playCount`, `paused`, `focused`) that sits beside, not inside,
  `vosu.client`.
- `vosu.osu.beatmap` keeps full osu! chart detail (difficulty stats, hit
  windows, convert flag, checksum, ids); the subset listed under
  [`vosu.chart`](#vosu-chart) is the same underlying data, additionally
  projected onto the common namespace.
- `vosu.osu.play` keeps osu!-specific play detail (mods, rank strings, pp,
  hit-timing arrays, `keys.all` source material, unstable rate) beside, not
  instead of, the common `vosu.play` fields this plugin fills (`score`,
  `combo`, `maxCombo`, `accuracy`, `health`, `progress`, `rate`, `judgements`).
- The 1.x `hits`/`statistics` numeric-keyed objects (`0`, `50`, `100`, `300`,
  `geki`, `katu`) are split in two: the six-judgement breakdown
  (miss/meh/ok/good/great/perfect) that every ruleset shares becomes the
  common `vosu.play.judgements` list (`vosu.osu` is the only plugin filling
  it in 2.0); osu!-only extras with no judgement-list equivalent (slider/hold
  ticks, bonuses, combo breaks, legacy combo-increase bookkeeping) stay in
  `vosu.osu.play.statistics`.
- 1.x's "current file" lookups (`folders`, `files`, `directPath`,
  `GET /Songs/{path}`, `GET /files/beatmap/*`, `GET /files/skin/*`) are
  removed from the 2.0 surface by ADR 0021 in favor of
  [resource references](#resource-references): chart files live at
  `vosu.chart.resources`, the skin's files at `vosu.skin.tree`. They remain
  exactly as-is in the upstream adapter; API v1 never serves a raw filesystem
  path.
- 1.x's always-on `keys.k1`/`k2`/`m1`/`m2` upstream-compatible members are
  replaced by `vosu.osu.keys.all` alone; the mode-specific `B1..BN` mapping
  that 1.x's `all` array already used (README "Renderer state (v2)" /
  `keys.all` table) is unchanged and is reproduced below under
  [`keys`](#keys-vosu-osu-keys).
- The opt-in flags `ENABLE_MANIA_KEY_HIT_ERRORS`, `ENABLE_HIT_OFFSETS`, and
  `ENABLE_KEY_OVERLAY`/`ENABLE_MANIA_KEY_HIT_ERRORS`-gated fields are removed
  (ADR 0021): subscribing to `vosu.osu.play.keyHitErrors`, `.hitOffsets`, or
  `vosu.osu.keys` is itself the opt-in, same as every other field.

### 1.x-to-2.0 field mapping

1.x paths below are JSON Pointers into the `/json/v2` document (`src/model/api/v2.rs`)
or, for `hitErrors`/`pressHitErrors`/`releaseHitErrors`/`keyHitErrors`/`hitOffsets`/`catcher`,
the `/json/v2/precise` document (`src/model/api/v2.rs::PreciseResponse`); both
carry the same values while subscribed, at different cadences.

| 1.x path | 2.0 path(s) | Notes |
| --- | --- | --- |
| `game.paused` | `vosu.osu.session.paused` | |
| `game.focused` | `vosu.osu.session.focused` | Windows-only; `null` elsewhere, as in 1.x. |
| `session.playTime` | `vosu.osu.session.playTime` | |
| `session.playCount` | `vosu.osu.session.playCount` | Always `0` in 1.x; kept for shape parity. |
| `state` | `vosu.status.status`, `vosu.osu.beatmap.status` | `state` was both the coarse status and `beatmap.status`'s numeric/name pair; 2.0 splits the coarse enum into the common namespace and keeps the raw `{number,name}` pair in `vosu.osu.beatmap.status` for osu!-specific consumers (results/edit/select variants the common enum collapses). |
| `rankedPlay` | `vosu.osu.rankedPlay` | |
| `profile` (all fields) | `vosu.osu.profile` | No common-layer equivalent; osu!'s bancho identity model does not generalize. |
| `beatmap.isKiai`, `.isBreak`, `.isConvert` | `vosu.osu.beatmap.isKiai`/`.isBreak`/`.isConvert` | |
| `beatmap.time.*` | `vosu.osu.beatmap.time.*` | `live` also feeds `vosu.play.progress`; `rate` also feeds `vosu.play.rate`. |
| `beatmap.status` | `vosu.osu.beatmap.status` | See `state` row above. |
| `beatmap.checksum`, `.id`, `.set` | `vosu.osu.beatmap.checksum`/`.id`/`.set` | |
| `beatmap.mode` | `vosu.osu.beatmap.mode` | |
| `beatmap.artist`, `.artistUnicode`, `.title`, `.titleUnicode`, `.mapper`, `.version` | same path under `vosu.osu.beatmap`, and `artist`/`title`/`mapper`/`version` additionally feed `vosu.chart.artist`/`.title`/`.charter`/`.difficultyName` | |
| `beatmap.source`, `.tags` | `vosu.osu.beatmap.source`/`.tags` | No common-layer equivalent. |
| `beatmap.stats.*` | `vosu.osu.beatmap.stats.*`; `bpm.common` also feeds `vosu.chart.bpm`; `objects.total` with the mode's key-count derivation feeds `vosu.chart.keyCount` for mania | |
| `play.source` | `vosu.osu.play.source` | |
| `play.failed` | `vosu.osu.play.failed` | |
| `play.playerName` | `vosu.osu.play.playerName` | |
| `play.mode` | `vosu.osu.play.mode` | |
| `play.score` | `vosu.play.score` | |
| `play.accuracy` | `vosu.play.accuracy`; `vosu.play.accuracyKind` is new, declaring the osu! formula per mode (`osu-standard`, `osu-taiko`, `osu-catch`, `osu-mania`) | |
| `play.healthBar` | `vosu.osu.play.healthBar`; `.normal` also feeds `vosu.play.health` | |
| `play.hits` (`Hits`/`LazerHits`) | `vosu.play.judgements` (the six-judgement breakdown) + `vosu.osu.play.statistics` (slider/hold ticks, bonuses, combo breaks, legacy combo increase) | See "Restructuring" above. |
| `play.hitErrorArray` | `vosu.osu.play.hitErrors.all` | |
| `play.pressHitErrors` | `vosu.osu.play.hitErrors.press` | |
| `play.releaseHitErrors` | `vosu.osu.play.hitErrors.release` | |
| `play.hitOffsets` | `vosu.osu.play.hitOffsets` | No longer gated by `ENABLE_HIT_OFFSETS`; subscribing opts in. |
| `play.keyHitErrors` | `vosu.osu.play.keyHitErrors` | No longer gated by `ENABLE_MANIA_KEY_HIT_ERRORS`. |
| `play.catcher` | `vosu.osu.play.catcher` | |
| `play.combo.current`, `.max` | `vosu.play.combo`, `vosu.play.maxCombo` | |
| `play.mods` | `vosu.osu.play.mods` | No common-layer equivalent; mod systems do not generalize. |
| `play.rank` | `vosu.osu.play.rank` | |
| `play.pp` | `vosu.osu.play.pp` | osu!-specific calculation; no common-layer equivalent. |
| `play.unstableRate` | `vosu.osu.play.unstableRate` | |
| `keys.k1`, `.k2`, `.m1`, `.m2` | removed; use `vosu.osu.keys.all` | See [`keys`](#keys-vosu-osu-keys). |
| `keys.all` | `vosu.osu.keys.all` | |
| `hitErrors`, `pressHitErrors`, `releaseHitErrors` (precise, tourney-nested) | `vosu.osu.tourney.clients[].play.hitErrors` | The precise document's top-level `tourney[]` becomes part of `vosu.osu.tourney.clients[]`; there is no separate precise document in 2.0 (ADR 0021). |
| `leaderboard[]` | `vosu.osu.leaderboard[]` | |
| `performance.accuracy`, `.graph` | `vosu.osu.performance.accuracy`/`.graph` | |
| `resultsScreen` | `vosu.osu.result` | Distinct from `vosu.status.status == "results"`, which only marks the coarse status. |
| `folders.*`, `files.*`, `directPath.*` | removed; use [resource references](#resource-references) under `vosu.chart.resources` and `vosu.skin.tree` | ADR 0021: no "current file" lookups in the 2.0 surface. Unchanged in the upstream adapter. |
| `settings.*` | `vosu.osu.settings.*` | `interfaceVisible`, `replayUIVisible`, `bassDensity` keep their [sampling-lane](#sampling-lanes) roles; `skin` sub-object is replaced by [`vosu.osu.skin`](#skin-vosu-skin-and-vosu-osu-skin) plus the common `vosu.skin.name`. |
| `room` | `vosu.osu.room` | |
| `tourney` | `vosu.osu.tourney` | |
| `GET /files/skin/` metadata (`client`, `name`, `files`, `builtin`, `base`, `entries[]`, `truncated`) | `vosu.skin.name` (common `name`); `vosu.osu.skin.{builtin,base,variant,files}`; `entries`/`truncated` are now the tree index at `vosu.skin.tree`'s `url` (`GET /api/v1/trees/{id}/`, ADR 0021) rather than a duplicated inline list | |

### `keys` (`vosu.osu.keys`)

`vosu.osu.keys.all` is the ordered logical-action list 1.x already published
at `keys.all`: each entry is `{name, isPressed, count, pressedAt, releasedAt}`,
with `name` a logical action (`B1` onward on both client variants, not a
physical key name). The ruleset-dependent order and meaning are unchanged
from 1.x:

| Ruleset | `all` order | Meaning |
| --- | --- | --- |
| osu! | `B1`, `B2`, `B3` | left button, right button, Smoke |
| taiko | `B1`-`B4` | left rim, left centre, right centre, right rim |
| catch | `B1`-`B3` | move left, move right, dash (left/right are never pressed by stable; dash is ordinary dash, exactly `1`) |
| mania | `B1` through `BN` | displayed column order; `N` is the converted map's key count, including key mods and dual-stage conversion |

### `skin` (`vosu.skin` and `vosu.osu.skin`)

`vosu.skin.name` and `vosu.skin.tree` carry the common projection. osu!-only
detail sits in `vosu.osu.skin`: `builtin` (one of `argon`, `argon-pro`,
`triangles`, `legacy`, `retro`, or `null` for an imported skin), `base` (the
same enum, naming the imported skin's rendering family; stable skins are
always `legacy`), `variant` (`stable`/`lazer`/`null`), and `files` (whether
the skin has any browsable files at all). Lazer's content-addressed skin
storage has no filesystem folder; `vosu.osu.settings.client`/compatibility
`folders.skin` keep their existing meaning in the upstream adapter only (ADR
0021) — API v1 consumers use `vosu.skin.tree` instead.

## Resource references

Per ADR 0021, state never carries a raw filesystem path. It carries
references that resolve through API v1:

- A **resource reference** is `{"url", "type"}`: `url` is
  `/api/v1/resources/{id}`; `type` is the response's MIME type from the
  route's allowlist.
- A **tree reference** is `{"url"}`: `url` is `/api/v1/trees/{id}/`, which
  lists the indexed folder; `GET`/`HEAD` on a path under it resolves one file.

Both shapes are defined once, in `schemas/common-defs.schema.json#/$defs/resourceRef`
and `#/$defs/treeRef`, and `$ref`'d from every namespace schema that carries
one: `vosu.chart.resources.{chart.file,chart.audio,chart.background}` and
`vosu.chart.resources["chart.set"]` (a tree), and `vosu.skin.tree`. A plugin
that defines its own resource kinds (`resource:<plugin-id>.<kind>`, per the
naming contract) places them in its own namespace using the same two shapes.

## Sampling lanes

ADR 0020's Publication section ties sampling rate to subscription demand: a
plugin samples a high-frequency value at its faster rate only while a client
is subscribed to it (and, for gameplay-only values, only while the session is
in `play` status). This generalizes 1.x's fixed selected-field demand
classifier (`V2SubscriptionHub`/the plan compiler, `src/server/v2_subscription.rs`),
which unions four demand flags — `regular`, `precise`, `gameplay_regular`,
`gameplay_precise` — from the set of subscribed JSON Pointers.

2.0 generalizes this per namespace field rather than per fixed pointer set, so
any plugin's own namespace can declare the same behavior. A property carries
an `x-sampling-lane` annotation (`schemas/common-defs.schema.json#/$defs/samplingLane`)
when selecting it changes reader demand:

```json
"x-sampling-lane": { "lane": "precise", "activity": "gameplay" }
```

- `lane` is `regular` (the plugin's normal-cadence reader), `precise` (its
  high-frequency reader), or `both` (demand on both; 1.x's unstable rate is
  the only such field, because it is computed from both lanes' data).
- `activity` is `always` (the lane runs at its accelerated rate whenever
  demanded, in any status) or `gameplay` (accelerated demand applies only
  while `vosu.status.status == "play"`; outside play the field still
  publishes, at the lane's idle/configured cadence).

A property with no `x-sampling-lane` annotation needs no extra reader demand:
subscribing to it does not change sampling behavior, equivalent to 1.x's
"None; configured polling continues unchanged" row. The compiler computing
demand for a subscription unions the `lane`/`activity` pairs of every selected
field's leaves, exactly as 1.x's pointer-demand union does; a parent pointer
(e.g. `vosu.osu.play`) selects the union of its descendants' lanes.

`vosu.osu.schema.json` annotates every field 1.x's pointer-demand classifier
names, translated through the [mapping table](#1x-to-20-field-mapping) above:
`vosu.status.status` and `vosu.osu.beatmap.time.live` are `precise`/`always`;
`vosu.osu.session.playTime`, `vosu.osu.settings.bassDensity`, and `vosu.osu.room`
are `regular`/`always`; the gameplay-only precise group (`vosu.osu.keys`,
`vosu.osu.play.hitErrors`, `.keyHitErrors`, `.hitOffsets`, `.catcher`) is
`precise`/`gameplay`; the gameplay-only regular group (`vosu.osu.play.source`
through `.pp`, `vosu.osu.leaderboard`, `.rankedPlay`, `.tourney`,
`vosu.osu.beatmap.{isKiai,isBreak,stats.stars.live,time.rate,time.running}`,
`vosu.osu.settings.{interfaceVisible,replayUIVisible}`, `vosu.osu.session.paused`)
is `regular`/`gameplay`; `vosu.osu.play.unstableRate` is `both`/`gameplay`. The
common `vosu.play.progress` and `.rate` inherit their source fields' lanes
(`precise`/`always` and `regular`/`gameplay` respectively), since they are
derived from the same underlying reads.

## Common-layer coverage: what the osu! plugin fills

| Field | Filled | Notes |
| --- | --- | --- |
| `vosu.status.status` | always | |
| `vosu.chart.title`, `.artist`, `.difficultyName`, `.charter`, `.length`, `.bpm` | always | |
| `vosu.chart.keyCount` | mania only | `null` for every other ruleset. |
| `vosu.chart.resources` | always when a chart is selected | Kinds with no file (e.g. no video) are omitted per kind, not nulled. |
| `vosu.play.score`, `.combo`, `.maxCombo`, `.accuracy`, `.accuracyKind`, `.progress`, `.rate` | during play (including replay/spectate/autoplay) | `null` outside play. |
| `vosu.play.health` | during play, all rulesets except where the client itself reports none | |
| `vosu.play.judgements` | during play | Always the full six-entry breakdown; a ruleset without a judgement (e.g. catch's hit window table) still reports `count: 0` for it, matching 1.x's all-zero `HitWindow` convention for catch. |
| `vosu.skin.name` | always | |
| `vosu.skin.tree` | when the active skin has browsable files | `null` for a built-in lazer skin. |

## Versioning and compatibility

Each namespace schema carries an independent semver `version`
(`schemas/*.schema.json`'s top-level `version` key), reported alongside the
namespace's capability id and version requirement
(`{"id": "field:<namespace>", "version": "<semver req>"}`, per the naming
contract; requirement syntax itself is specified in `docs/v2/catalog-v2.md`).

- **Additive, non-breaking**: a new optional field, a new enum value a
  consumer can treat as unknown-and-ignore, a new declared resource/tree kind,
  or a new `x-sampling-lane` annotation on a previously-unannotated field.
  Bumps the namespace's minor version. Existing consumers remain valid
  without a resolver change.
- **Breaking**: removing or renaming a field, changing a field's type or
  required-ness, changing the meaning of an existing value (including
  reordering `judgements` ordinals or redefining a `status` enum value),
  or changing a resource/tree reference's shape. Bumps the namespace's major
  version. A plugin publishing a new major version keeps serving the prior
  major version's shape at its old capability version until every first-party
  consumer has migrated (ADR 0022's transition policy governs the schedule
  for official plugins and overlays); third-party consumers pin a version
  requirement and the resolver fails closed rather than silently serving a
  mismatched shape.
- Namespace versions are independent: a major bump to `vosu.osu` does not
  require bumping `vosu.client` or the other common namespaces, and vice
  versa.
- `common-defs.schema.json`'s `$defs` are part of every namespace that
  `$ref`s them. Changing a shared `$def` in a breaking way is a breaking
  change for every namespace that references it, and every affected
  namespace's major version bumps together in the same change.

## Open questions

- **Judgement id vocabulary.** This spec declares the osu! plugin's six
  `vosu.play.judgements` ids (`perfect`, `great`, `good`, `ok`, `meh`, `miss`)
  but ADR 0020 does not standardize a shared vocabulary across games, by
  design (each game declares its own). An overlay author who wants one
  "accuracy-equivalent" bar across games has no portable way to know, for an
  unfamiliar game's namespace, which `ordinal` range counts as "good" versus
  "bad" beyond the ordinal ordering itself. Recommendation: leave this to
  catalog-level overlay compatibility metadata (CatalogV2Spec) rather than
  the state schema — a declared `ordinal` is sufficient for the generic
  "best-to-worst" bar ADR 0020 describes, and inventing a cross-game id
  vocabulary risks the "normalized judgements" alternative ADR 0020 already
  rejected.
- **`vosu.osu.result` versus a generic `vosu.status == "results"` payload.**
  The results screen is entirely osu!-specific today (score id, pp, mods).
  If a future common namespace wanted a minimal cross-game "last result"
  summary (final score/accuracy/rank), it would need its own ADR; this spec
  does not add one, since ADR 0020 lists only `status`/`chart`/`play`/`skin`
  as common namespaces. Recommendation: defer until a second game plugin
  exists and a concrete overlay need appears.
- **`vosu.chart.keyCount` for non-lane games.** The field is `null` for any
  ruleset without a fixed lane count (including non-mania osu! rulesets).
  Whether a future game plugin with a different "lane-like" concept (e.g. a
  variable number of simultaneous tracks) should reuse this field or declare
  its own is left to that plugin's design; recommendation is to reuse it only
  when the concept is genuinely a fixed per-chart lane count, and otherwise
  leave it `null` and expose the richer concept in the game's own namespace.
