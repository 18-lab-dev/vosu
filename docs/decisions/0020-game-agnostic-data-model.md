# Publish game state as owned namespaces with a best-effort common layer

- Status: Proposed
- Date: 2026-11-21
- Supersedes: [ADR 0001](0001-single-daemon-multi-instance-lifecycle.md) in part (its osu!-only scope; deterministic generations and selection remain)

## Context

1.x state is shaped by osu!'s clients and the tosu-compatible schemas. 2.0 game
plugins ([ADR 0019](0019-plugin-runtime-and-trust.md)) can describe other rhythm
games, and an overlay that shows only universal values such as combo or accuracy
should work for any of them. Those values are not uniform: judgement names and
counts, accuracy formulas, and rate modifiers differ between games, and some
plugins will not be able to read every value.

## Decision

### Namespaces

State is a set of namespaces. Each namespace has one owner and a published JSON
Schema with a semver version.

| Namespace | Owner | Filled by game plugins |
|---|---|---|
| `vosu.client` | core | required |
| `vosu.status`, `vosu.chart`, `vosu.play`, `vosu.skin` | core | best effort, declared |
| `vosu.osu` | the official osu! plugin | that plugin's contract |
| `<plugin-id>` and below | that plugin | that plugin's contract |

The `vosu.` prefix is reserved for core and official plugins. Public
documentation names only the namespaces of publicly distributed plugins.

`vosu.client` identifies the attached client: game identifier, client variant
(for example `stable` or `lazer`), display name, version, process ID, session
ID, whether it is focused, and the providing plugin's identifier and version.

### The common layer

`vosu.status`, `vosu.chart`, `vosu.play`, and `vosu.skin` hold values most
rhythm games share: status (menu, selection, play, pause, results), chart
metadata (title, artist, difficulty name, charter, length, BPM, key count),
play values (score, combo, maximum combo, accuracy, health, progress, rate), and
the current skin.

- Judgements are a list the game plugin declares, each with an identifier, a
  display name, and an ordinal from best to miss. Core does not map one game's
  judgements onto another's.
- Accuracy carries `accuracy_kind`, naming the game's own formula, rather than
  being normalized.
- Filling the common layer is best effort, but declared: each game plugin's
  manifest lists the common fields it fills. A field the plugin does not fill
  is `null`, never a zero or empty placeholder.

Overlays and tools declare the fields they require. Because coverage is
declared, the dashboard can state which games an overlay works with before it is
installed, and the install resolver can find a providing plugin
([ADR 0022](0022-install-resolver-and-2-0-transition.md)).

### Sessions

Each attached client is a session. Discovery, generations, and deterministic
selection follow ADR 0001 per game plugin; core adds focus selection across
games. Consumers address the focused session by default or any session by ID.

### Publication

Plugins publish typed values to the state bus, not serialized JSON. Core keeps
ADR 0002's immutable, latest-only publication per session and serializes and
projects for each API. Field subscriptions drive sampling demand, as the 1.x
selected-field subscription hub does, so a plugin samples a high-frequency value
only while someone subscribes to it.

## Consequences

- An overlay that uses only covered common fields works across games without
  code for each game; game-specific overlays use the plugin's namespace.
- Common fields cannot express every game's nuance; that nuance stays in the
  game's namespace by design.
- Schema versions make breaking namespace changes visible to consumers and to
  the resolver.

## Alternatives considered

- **A mandatory universal schema:** rejected; plugins that cannot read a value
  would fill misleading placeholders.
- **Game namespaces only:** rejected; no overlay could work across games.
- **Normalized judgements and accuracy:** rejected; any mapping misstates at
  least one game's scoring.
