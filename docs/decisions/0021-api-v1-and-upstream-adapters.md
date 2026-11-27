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

## Amendments

**2026-11-21** (A1, adversarial review of the implementation plan; full
rationale in `docs/v2/implementation-plan.md` section 10):

- **D2 Plugin-only paths.** "State carries references" (above, line 43)
  holds for every ordinary field reaching API v1, with one narrow,
  schema-declared carve-out ([ADR 0020](0020-game-agnostic-data-model.md)'s
  own amendment): `vosu.osu`'s `internal.paths` subtree, which core's
  projector strips from every API v1 document, subscription, and session
  event **unconditionally** — never gated by a consumer's declared
  capabilities, which cannot reach API v1 at all. A plugin's own ABI reads
  of it are instead gated by `requires`/`optional` naming
  `field:vosu.osu.internal` or a descendant
  (`docs/v2/state-schema.md`'s `internal` section is authoritative). Every
  adapter file route in this document (`GET /Songs/{path}`,
  `GET /files/skin/{path}`, and the rest) resolves against the osu!
  plugin's own `ROOT_DECLARE` handles (`docs/v2/plugin-abi.md` §6.5), never
  against a path string read off the bus, so an adapter needs no grant onto
  `internal` to serve them. The streamCompanion adapter's upstream-only
  derived values (`src/model/api/sc.rs:132-141`) are computed by that
  adapter itself from `vosu-osu-calc` and the chart resource, not read back
  out of `internal` either.
- **D3 Bundled-only HTTP surfaces.** "Plugins never serve HTTP" applies to
  every plugin that is not bundled, without exception; the dashboard,
  overlay hosting, tool hosting, and these adapters are the one documented
  exception, using the in-process-only `vosu-surface` interface — listed
  exhaustively in `docs/v2/plugin-abi.md` §7.5 and
  [ADR 0019](0019-plugin-runtime-and-trust.md)'s D3 amendment: route and
  WebSocket-feed registration, subscription-gated state reads,
  `ROOT_DECLARE`-confined file responses, and management-route provenance;
  no `ProcessMemory`, configuration writes, or credentials.
- **D10 Names.** The three adapters above are named, as crates and plugin
  ids, `vosu-adapter-upstream`/`vosu.adapter.upstream`,
  `vosu-adapter-gosumemory`/`vosu.adapter.gosumemory`, and
  `vosu-adapter-streamcompanion`/`vosu.adapter.streamcompanion` — not a
  bundled `vosu.tools`/`vosu.overlays`-shaped id, which would block the
  ecosystem's natural `vosu.tools.<name>` ids under the id-extension rule
  (`docs/v2/catalog-v2.md`). "Adapter names are upstream identifiers and
  need `upstream` entries in `scripts/brand-allowlist.txt`" above is
  superseded for the plugin/crate names themselves: they now avoid the old
  brand entirely, so only this ADR's own prose about the projects they
  imitate still needs those allowlist entries.
- **D20 1.x overlays using removed APIs.** "Overlays written for vosu use
  API v1; overlays written for tosu run unchanged through the adapters"
  above describes the upstream-compatible surfaces only; a 1.x-era *vosu*
  overlay built from the quickstart used vosu's own
  `/websocket/v2/subscribe` addition, which M4a removes — see
  `docs/v2/catalog-v2.md`'s amended "User overlays" migration entry for the
  resulting notice-and-attribution behavior.
- **D6 Discovery parity and divergence-file classes (third/fourth
  re-review, F2/F3/G1).** `vosu.osu.tourney` is `null` exactly where 1.x
  omits the `tourney` key (the lazer multi-spectating path before the
  first lobby read lands); every upstream-compatible adapter that carries
  `tourney` (the upstream v2 adapter and the gosuMemory v1 adapter, both
  of which 1.x also omits the key for) drops the key from its wire output
  when the published value is `null`, matching 1.x's own omission.
  "Adapters diverge only for security" (above, line 87) is amended: every
  divergence-file entry (above, and `/api/ingame`'s, line 101) carries one
  of four classes — `security` (the sandbox header, path confinement, peer
  and Origin policy, request limits: permanent), `contract` (the ADR 0004
  legacy calculation endpoint's separate contract, line 90: permanent),
  `licensing` (`/api/ingame`'s functional-rather-than-byte-identical
  compatibility, line 101: permanent, since vosu cannot ship upstream's
  LGPL assets), and `temporary` (a pre-release gap with a task that closes
  it, newly adopted here: must be gone before the day-0 release gate
  passes, `docs/v2/catalog-v2.md` "Day-0 release check"). The gate fails
  closed when a `temporary` entry remains, or when any entry's class is
  missing or is none of the four defined classes; the three permanent
  classes, correctly recorded, never fail it, so the file keeping
  `/api/ingame`'s and ADR
  0004's entries forever does not block release. The one `temporary` entry
  adopted at this freeze is the stable tournament-manager aggregate
  (`docs/v2/implementation-plan.md`'s M4a-or-later task): until it lands,
  `stable-tournament`'s `tourney` members (`v1`, `v2`, `v2-precise`) are
  exempted from the M2/M4a byte-for-byte exits. Why: deferring the
  aggregate indefinitely (the first A1 review round's resolution) made
  those exit tests impossible to pass against the committed oracle, since
  `stable-tournament` encodes real manager and spectator data; a gate that
  failed on any non-security entry could never pass, since `/api/ingame`'s
  and ADR 0004's entries are permanent and not about security; per-entry
  classes let the gate target exactly the pre-release gaps it should.

**2026-11-24 (A4):** The
[source fidelity/removal inventory](../../tests/fixtures/upstream/ADAPTER_CONTRACT.md)
and [permanent bundled surface contract](../v2/adapter-surfaces.md) resolve
the approved M4a clarifications. They supersede the D6 statement equating
all tournament nulls with omission: pending data omits the upstream key,
missing-score data produces the pinned adapter failure, while API v1 remains
accurate. Independent legacy input slots are public typed facts, never
guessed from logical actions. The stable manager is mandatory for completed
M4a; its temporary exemption is closed only with implementation evidence.
The endpoint inventory removes extension copies from every writer and nested
tournament path without removing legitimate upstream all-hit histories.
Legacy calculation accepts relative declared-root children and absolute
spellings confined inside the selected session's declared roots only; the
new rejection is a named security divergence. D20 attribution uses validated
overlay identity, bounded deduplication and separate unknown buckets, never
Origin alone or arbitrary path/referrer logging.

**2026-11-24 (A4b):** The
[behavioral golden contract](../../tests/fixtures/upstream/GOLDEN_CONTRACT.md)
makes "commits the captures as golden fixtures" (above, line 84) precise.
Raw live captures stay private and are never committed or uploaded; a
`golden` command normalizes only a closed list of exact-scope volatile values
and writes a candidate that main reviews before commit, using generated
vosu-owned test media only. Fixtures live under
`tests/fixtures/upstream/<pin>/`, the test reads only the current pin's
directory, and a pin bump deletes the previous one and recaptures every
platform. "Each divergence is listed with its reason in one file" (line 87)
is extended: compatibility tests subtract a ledger row only through exact
machine-readable rules in `divergence-rules.json` that name an existing
non-`temporary` ledger ID, the affected cases and record part; unused rules
and whole-route subtraction without whole-route ledger scope fail. The
`/api/ingame` route is never captured or used as a golden.

**2026-11-24** (AF, ecosystem fan-out contract freeze; full record in
[`docs/v2/ecosystem-release-matrix.md`](../v2/ecosystem-release-matrix.md)):

- **Job artifacts (AF.12).** Resources stay what this ADR says: references
  published in session state, public under peer policy. A sessionless or
  protected output (an offline export, a stored replay, a rendered video) is
  not a resource. It is an artifact owned by one action job, served only at
  `GET`/`HEAD /api/v1/jobs/{job}/artifacts/{artifact}` to that job's own
  launch session or the broker, with `no-store` and the job's retention as
  its lifetime ([`docs/v2/api-v1.md`](../v2/api-v1.md#job-artifacts-af12)).
  Formerly private files never become public resources, and no fake session
  is created to publish one.
- **Long work and uploads (AF.3, AF.6).** The 30-second invocation deadline
  stands; long work is a plugin-owned operation behind short actions, and
  browser uploads use bounded staged upload actions, both specified in
  `docs/v2/api-v1.md`.
- **Upstream compatibility inside a private backend (AF.13).** The replay
  renderer's private replay-overlay backend may serve upstream-compatible
  routes only to render third-party overlays inside a replay; it is not a
  vosu adapter, registers nothing with core and is never a first-party
  fallback. Migrated v2 overlays read replay state there through API v1
  playback semantics.
- **`/api/ingame` successor (A7.5).** The dashboard banner's install action
  targets the `kind = "tool"` entry `vosulay-editor` in
  `18-lab-dev/vosulay`'s `catalog-v2.json`, whose requirements bring in the
  `vosu.vosulay` supervisor through the normal install plan.

**2026-11-24** (A7.4, A7.5, M7 contract freeze; full text in
[adapter surfaces](../v2/adapter-surfaces.md#a74-d20-notice-presentation)):

- **D20 notice (A7.4).** The A4.5 recorder and its limits are reused
  unchanged. The dashboard renders its bounded notices from the existing
  status read, with escaped identities, elapsed ages and a fixed
  migration-guide link, and with no timer, persistence or dismissal.
- **`/api/ingame` deprecation (A7.5).** "The first request logs one warning"
  means one warning per daemon process. "While the endpoint is in use" means
  while an in-memory last-use marker is live, expiring after 24 hours
  without a request and recording no URL, IP or referrer. The install target
  named in the AF amendment above is the A7.5 target. The button starts the
  ordinary install plan with every approval the plan lists; it never
  approves anything itself. The endpoint is still landing G, which needs the
  eligible author's behavioral document first. Until G lands, the endpoint is
  outstanding, not complete or waived.

**2026-11-25** (owner decision; full text in
[adapter surfaces](../v2/adapter-surfaces.md#a75-apiingame-removed-route-notice)):

- **`/api/ingame` is removed from 2.0.** The owner withdrew the clean-room
  requirement. vosu 2.0 serves no `/api/ingame` page of any origin, so no
  behavioral specification, eligible author, second implementer or
  landing G exists. This supersedes the "`/api/ingame`" section above
  (line 93), the rejected "Dropping `/api/ingame`" alternative (line 131),
  and the deprecation headers, banner and editing-mode notice of the A7.5
  amendment above. The supported in-game overlay path is
  [vosulay](https://github.com/18-lab-dev/vosulay): the official
  `vosulay-editor` tool, whose install plan adds the `vosu.vosulay`
  supervisor ([ADR 0008](0008-external-in-game-overlays.md)).
- **A D20 removed route.** `/api/ingame` is handled like
  `/websocket/v2/subscribe` and `/files/skin/`: every method gets the
  ordinary exact `Not Found` 404. It never upgrades, redirects, falls
  through to static files, or carries a `Deprecation` or `Link` header.
  The response is 404 and not 410 for three reasons. A4.5 fixes the
  ordinary 404 for every removed route. In this repository, 410 means only
  an authentic but retired API v1 resource, tree or artifact id. And 1.x
  has answered 404 since it removed the route, so existing clients see no
  change. The unchanged A4.5 recorder attributes these requests under one
  new fixed feature, `ingame-page`.
- **Notice semantics (A7.5 rewritten).** The adopted limits stay the same:
  one WARN per daemon process, on the first request to the removed route,
  and a 24-hour inactivity expiry, which is now the recorder's own TTL
  rather than a separate marker. The dashboard shows the D20 notice for
  `ingame-page`, including for the unknown bucket. Its one action starts
  the A7.5 vosulay install through the ordinary plan, approval and trust
  flow, and never approves anything itself.
- **Divergence class.** The permanent `licensing` class now covers this
  removal (`adr0021-removed-ingame-page`): vosu cannot ship upstream's LGPL
  page, and 2.0 has no replacement. The four classes and the day-0 rule are
  unchanged. `/api/ingame` is still never captured or used as a golden.

**2026-11-26 (owner decision):** Upstream compatibility is payload
compatibility, not wire spelling. The adapters still reproduce fields,
values, quirks, status codes and error bodies bug for bug. HTTP JSON and
WebSocket JSON text frames are compared after parsing, including error
bodies. Non-JSON payloads stay byte-exact. The
[golden contract §7](../../tests/fixtures/upstream/GOLDEN_CONTRACT.md#7-normalization-inventory-a4b6)
defines payload equality, including the scoped 250 ms live-clock bound.
Its §6 compares only the media type (ignoring charset), `Content-Range`,
`Location`, and `Accept-Ranges`. Date, connection, upgrade, keep-alive,
transfer coding and WebSocket handshake headers are not payloads.
`Content-Length` checks each response's own framing. CORS, CSP, nosniff,
`Cache-Control`, `ETag` and `Last-Modified` remain governed by vosu's own
security and contract tests, not by upstream behavior.
