# Serve vosu API v1 and keep upstream APIs as exact adapters

- Status: Accepted (vosu 2.0)
- Date: 2026-11-21
- Extends: [ADR 0004](0004-separate-calculation-contracts.md), [ADR 0005](0005-root-confined-file-serving.md), [ADR 0011](0011-live-parity-optional.md)

## Context

1.x serves gosumemory v1, StreamCompanion, tosu v2, and v2 precise, and adds
vosu-only fields and a selected-field subscription endpoint to the tosu v2
surface. Overlays that use those additions work only on vosu, while overlays
written for tosu never see them. File routes return "the current" beatmap or
skin file, so a selection change during a fetch can return another map's file;
the README tells consumers to compare MD5 checksums to detect it.

2.0 needs one API for its own data model ([ADR 0020](0020-game-agnostic-data-model.md))
and compatibility surfaces that behave exactly like the projects they imitate.

## Decision

### API v1

vosu's own API lives under `/api/v1`. It is named "vosu API v1"; source code
does not reuse `v1` for the gosumemory surface.

- HTTP reads and WebSocket subscriptions address sessions:
  `/api/v1/sessions`, and `/api/v1/sessions/{id|active}/…`.
- Subscriptions select fields with JSON Pointers and keep 1.x filter semantics:
  bounded frames, latest-state backpressure, and demand-driven sampling.
- There is no separate precise endpoint. High-frequency fields are delivered on
  the same subscription at their own sampling rate while subscribed.
- The 1.x opt-in flags `ENABLE_MANIA_KEY_HIT_ERRORS` and `ENABLE_HIT_OFFSETS`
  are removed; subscribing to a field is its opt-in.

### Resources

Files are resources the state refers to, never "current file" lookups.

- A **resource** is one file; a **tree** is an indexed folder such as the
  current skin or a chart's set folder. Common kinds are `chart.file`,
  `chart.audio`, `chart.background`, `chart.set`, and `skin`; plugins add kinds
  in their own namespace. Game plugins declare the kinds they fill.
- State carries references: `{"url": "/api/v1/resources/<id>", "type": …}` or
  `{"url": "/api/v1/trees/<id>/"}`.

| Route | Serves | Caching |
|---|---|---|
| `GET`/`HEAD /api/v1/resources/{id}` | one file, single byte ranges | immutable |
| `GET /api/v1/trees/{id}/` | index of `path`, `size`, optional `hash`; `truncated`; `revision` | strong ETag, revalidated |
| `GET`/`HEAD /api/v1/trees/{id}/{path}` | a file, using the plugin's lookup rules | strong ETag, revalidated |
| `GET /api/v1/sessions/{id\|active}/chart/{kind}`, `/sessions/{…}/skin/{path}` | `307` to the current resource | `no-store` |

- Core mints IDs when a plugin publishes a reference: an HMAC under a persisted
  core secret of the plugin, its key, and the content identity (a store hash,
  or path, size, and modification time). IDs are stable across restarts and
  unguessable. Core serves only IDs that are currently published, from a
  bounded registry, so v1 never serves a file that the state or a published
  tree index did not reference. There is no route for browsing a game folder.
- Plugins resolve a key to either a declared root and relative path, which core
  opens with ADR 0005's retained-handle confinement, or a bounded byte stream
  for generated content and `process` plugins. Plugins never serve HTTP.
- Every resource and tree response carries
  `Content-Security-Policy: sandbox` and `X-Content-Type-Options: nosniff`. The
  type comes from an allowlist of image, audio, video, and chart text types;
  anything else, including HTML, is `application/octet-stream`.
- Resources are read-only. Changes go through plugin actions.

### Upstream adapters

The tosu v2 (including `/json/v2/precise` and `/websocket/v2/precise`, file
routes, `/Songs/{path}`, `/backgroundImage`, and the legacy calculation
endpoint), gosumemory v1, and StreamCompanion adapters reproduce a pinned tosu
revision **bug for bug**: fields, values, quirks, status codes, and error
bodies. A newer upstream behavior arrives only through a deliberate pin bump.

- The 1.x additions are removed from these surfaces: `pressHitErrors`,
  `releaseHitErrors`, `keyHitErrors`, `keys.all`, `catcher`, `hitOffsets`, the
  skin metadata index at `/files/skin/`, and `/websocket/v2/subscribe`. Their
  data is available through API v1.
- Upstream quirks are implemented in the adapters. The osu! plugin publishes
  accurate data, and API v1 never inherits an upstream defect.
- 1.x improvements over upstream also leave the adapters: `/files/skin/{path}`
  returns 404 on lazer as upstream does; API v1 trees serve lazer skins.
- Each pin bump captures the pinned upstream with `tools/live-parity` and
  commits the captures as golden fixtures; compatibility tests compare adapter
  output against them in CI. Live parity itself stays an optional diagnostic.
- Adapters diverge only for security, and each divergence is listed with its
  reason in one file next to the fixtures: the game-file `sandbox` header,
  path confinement, peer and Origin policy, and request limits. The legacy
  calculation endpoint keeps ADR 0004's separate contract inside the tosu
  adapter.

### `/api/ingame`

The tosu adapter keeps `/api/ingame`, the page upstream's in-game overlay loads,
as a vosu implementation. vosu does not ship upstream's page: its assets are
LGPL-3.0 and outside the relicensing permission recorded in
[THIRD_PARTY_NOTICES](../../THIRD_PARTY_NOTICES.md). The implementation is
written from a behavioral specification of what clients of the endpoint observe
and rely on, by contributors who have not read the upstream page assets. It is
functionally compatible rather than byte-identical, which the divergence file
records.

The endpoint is deprecated in favor of [vosulay](https://github.com/18-lab-dev/vosulay):
responses carry `Deprecation` and `Link: <…>; rel="successor-version"`
headers, the first request logs one warning, the page shows a notice in its
editing mode (never over gameplay), and the dashboard shows a banner with a
one-click vosulay install while the endpoint is in use.

## Consequences

- Overlays written for vosu use API v1; overlays written for tosu run unchanged
  through the adapters, including their dependence on upstream quirks.
- Every first-party consumer of the 1.x additions and of `/websocket/v2/subscribe`
  migrates to API v1 before release
  ([ADR 0022](0022-install-resolver-and-2-0-transition.md)).
- Resource URLs are cacheable and cannot race a selection change.
- Adapter correctness needs recorded upstream fixtures, refreshed on each pin bump.
- Adapter names are upstream identifiers and need `upstream` entries in
  `scripts/brand-allowlist.txt`.

## Alternatives considered

- **Keep vosu additions on the tosu surface:** rejected; it splits overlays into
  vosu-only and tosu-only variants of one API.
- **Adapters that are compatible in shape but correct upstream defects:**
  rejected; consumers that rely on an upstream quirk would break without
  warning.
- **Serving upstream's `/api/ingame` assets:** rejected; it would reintroduce
  LGPL material that vosu removed.
- **Dropping `/api/ingame`:** rejected; existing in-game setups would break
  without a migration path.
