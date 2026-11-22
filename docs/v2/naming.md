---
type: Contract
title: "vosu 2.0 naming contract"
description: "Plugin ids, namespaces, capability id grammar, reserved names, kinds, game ids, and the vosu.client fields every other vosu 2.0 spec cites as 'the naming contract'."
---

# vosu 2.0 naming contract

Every other 2.0 spec — [`docs/v2/plugin-abi.md`](plugin-abi.md),
[`docs/v2/state-schema.md`](state-schema.md),
[`docs/v2/api-v1.md`](api-v1.md), [`docs/v2/catalog-v2.md`](catalog-v2.md),
and `docs/v2/schemas/*.schema.json` — cites "the naming contract" for the
identifier grammar below rather than restating it; this is that single
source (`docs/v2/implementation-plan.md` gap 8, closed by A1). It defines
grammar and reservation, not behavior: how a capability id resolves to a
provider, how a namespace's fields are covered, and how a session is
discovered and selected are each owned by the spec that defines that
behavior and are not restated here.

## Plugin ids

A plugin id is 2–4 dot-separated segments, each matching
`[a-z0-9][a-z0-9-]{0,31}`: `^[a-z0-9][a-z0-9-]{0,31}(\.[a-z0-9][a-z0-9-]{0,31}){1,3}$`
(`schemas/common-defs.schema.json#/$defs/pluginId`). A tool or overlay id is
a single segment of the same per-segment grammar,
`^[a-z0-9][a-z0-9-]{0,31}$` — the unchanged 1.x tool/overlay id grammar —
and has no cross-registration rule with other ids. A plugin id must not
extend, or be extended by, another registered plugin id (bundled, installed,
or catalogued from any enabled source) or a core-owned namespace; doing so
is rejected at install time and at catalog validation time, since it would
make the longest-registered-namespace parse below ambiguous
(`docs/v2/catalog-v2.md` "Namespace and kind parsing" owns this algorithm).

A plugin id that equals or extends any core-owned namespace id (`vosu.client`,
`vosu.status`, `vosu.chart`, `vosu.play`, `vosu.skin`, `vosu.sessions`) is
always rejected, from any source, official included — those ids are never
a plugin id. Beyond that, a plugin id merely starting with the segment
`vosu` (without matching a core-owned namespace, e.g. `vosu.osu`,
`vosu.dashboard`) is reserved to official plugins and is rejected only from
a non-official source. A plugin id must also not start with the segment
`chart` or `skin`, so that a `resource:` id stays unambiguous between a
core kind and a plugin's own (below).

## Namespaces

State is a set of namespaces, each with one owner and a published JSON
Schema carrying its own semver `version`
(`docs/v2/state-schema.md`, [ADR 0020](../decisions/0020-game-agnostic-data-model.md)):

| Namespace | Owner | Reserved |
| --- | --- | --- |
| `vosu.client` | core | required for every attached session |
| `vosu.status`, `vosu.chart`, `vosu.play`, `vosu.skin` | core | best effort, declared by game plugins |
| `vosu.osu` | the official osu! plugin | that plugin's contract |
| `<plugin-id>` and below | that plugin | that plugin's contract |
| `vosu.sessions` | core | reserved pseudo-namespace, below |

`vosu.sessions` is reserved for session-lifecycle events delivered to a
subscribing plugin (`docs/v2/plugin-abi.md` §6.2); it is never a plugin id,
a published data namespace, or a `field:`/`resource:`/`action:` capability,
in any manifest, schema, or catalog entry.

## Capability id grammar

A capability id is one reserved prefix plus a dotted path, used in
manifests (`[[provides]]`/`[[requires]]`/`[[optional]]`) and in
`catalog-v2.json`:

| Prefix | Addresses | Example |
| --- | --- | --- |
| `field:` | a namespace (bare) or one field below it | `field:vosu.osu`, `field:vosu.play.combo` |
| `resource:` | a core resource kind (bare, `chart.`/`skin` only) or a plugin's own kind | `resource:chart.background`, `resource:acme.game.replay` |
| `action:` | one plugin action | `action:vosu.osu.beatmap.import` |
| `game:` | a game a plugin supports | `game:osu` |

Parsing a `field:`/`action:`/`resource:` id's namespace boundary from the
string alone is ambiguous (plugin ids are themselves 2–4 dotted segments);
the resolver parses it by the longest registered namespace that prefixes
the id, which `docs/v2/catalog-v2.md` "Namespace and kind parsing" and
"Capability coverage" own in full, including the ancestor-path coverage
rule a `provides` entry satisfies a `requires`/`optional` entry under.

## Reserved names

- Core resource kinds, bare (never a plugin-id prefix in `[[resource_kinds]]`
  or ABI `kind` fields; the plugin-id prefix appears only in the
  `resource:` capability id, and only for a plugin's own kind): `file`,
  `audio`, `background`, `set`, `skin`.
- Core namespaces (never satisfiable as a plugin id, and never requiring
  installation — "Built-in providers", `docs/v2/catalog-v2.md`):
  `vosu.client`, `vosu.status`, `vosu.chart`, `vosu.play`, `vosu.skin`,
  `vosu.sessions`.

## Kinds

- A resource kind is one segment, `[a-z0-9][a-z0-9-]{0,31}`
  (`docs/v2/plugin-abi.md` §6.5, `docs/v2/catalog-v2.md`).
- An action name is dot-segments, each `[a-z0-9][a-z0-9-]{0,31}`
  (`docs/v2/catalog-v2.md` `[[actions]]`); the full capability id is
  `action:<plugin-id>.<name>`.
- A judgement id is `^[a-z][a-z0-9-]{0,31}$`
  (`schemas/common-defs.schema.json#/$defs/judgement/properties/id`).

## Game ids

`game:<game-id>` uses `^[a-z][a-z0-9-]{0,31}$`, matching `vosu.client.game`
(`schemas/vosu.client.schema.json`, `docs/v2/catalog-v2.md` "Game
capabilities"). Every `provides`/`requires`/`optional` entry for a `game:`
id carries the literal version `"*"`: game support is a presence check, not
a version negotiation.

## `vosu.client` fields

The required namespace every attached session publishes, in this order
(`docs/v2/state-schema.md`, `docs/v2/api-v1.md` "Sessions"):
`game`, `variant`, `name`, `version`, `pid`, `session`, `focused`,
`plugin.{id,version}`. `game`, `variant`, `name`, and `version` are filled
by the attaching game plugin's `SESSION_IDENTIFY`; core fills `pid`,
`session`, `focused`, and `plugin` itself from its own bookkeeping
(`docs/v2/plugin-abi.md` §6.2).

## Session identity

A session has two identities, deliberately not interchangeable
(`docs/v2/plugin-abi.md` §3 "Session identity"):

- The **public id** (`vosu.client.session`): an opaque string core mints,
  used by every external consumer — `docs/v2/api-v1.md`'s `{id|active}`
  route segments, session-events subscriptions.
- The **handle** (`vosu_session_id`, a `uint64_t`): scoped to one plugin
  instance, carried on every ABI message that names a session. `0` means
  "no particular session" or "every session of this plugin" where an
  opcode's description says so.

A session's lifecycle, generation assignment, and deterministic selection
are [ADR 0001](../decisions/0001-single-daemon-multi-instance-lifecycle.md)'s,
generalized across game plugins by
[ADR 0020](../decisions/0020-game-agnostic-data-model.md); this document
only names the identifiers involved, not the discovery or selection
behavior itself.
