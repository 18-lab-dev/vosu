# vosu API v1

This is the implementer specification for vosu's own 2.0 API, decided by
[ADR 0021](../decisions/0021-api-v1-and-upstream-adapters.md). It is binding
for core, for the osu! plugin, and for overlay and tool authors building
against `/api/v1`. Field paths reference `docs/v2/state-schema.md`; manifest
capability declarations (`provides`/`requires`/`optional`) reference
`docs/v2/catalog-v2.md`. Both are owned by other specs and are not restated
here.

API v1 is served on the **overlay listener** (`SERVER_IP:SERVER_PORT`), next to
the upstream-compatible adapters and other overlay-origin routes
([ADR 0016](../decisions/0016-separate-dashboard-origin.md)). The dashboard
listener never serves API v1; dashboard management of API keys is documented
in the [README](../../README.md#api-keys) and unchanged by this spec.

## Sessions

Each attached game client is a session, discovered and selected per
[ADR 0001](../decisions/0001-single-daemon-multi-instance-lifecycle.md) and
[ADR 0020](../decisions/0020-game-agnostic-data-model.md). A session id is an
opaque core-minted string, and core never mints the literal strings `active`
or `subscribe` as one, so every `{id|active}` route segment and the literal
`/subscribe` path stay unambiguous. `active` addresses the focused session
wherever a route accepts `{id|active}`; addressing `active` when no session is
focused behaves as an unknown id (see status codes below).

A session summary is exactly the `vosu.client` fields from the naming
contract, in that order:

```json
{
  "game": "string",
  "variant": "string",
  "name": "string",
  "version": "string",
  "pid": 0,
  "session": "string",
  "focused": true,
  "plugin": { "id": "string", "version": "semver string" }
}
```

`session` is the session's own id — the same string every `{id|active}` route
segment above addresses it by.

### Focus rule

The rule ranges over identified sessions only (see Visibility below). Exactly
one identified session is focused whenever at least one exists: the session
whose process owns the OS foreground window, or, when no identified session's
process does, whichever session was most recently focused. The first focus
decision after no session was focused (daemon start, or every session having
ended before a new one is identified) picks the first session by
[ADR 0001](../decisions/0001-single-daemon-multi-instance-lifecycle.md)'s
deterministic ordering within the earliest-attached plugin. `vosu.client.focused`
is `true` for exactly that one session's summary; `active` always addresses it.

### Visibility

A session is not exposed anywhere in this API — `GET /api/v1/sessions`, the
`{id|active}` routes, session-events subscription, or focus selection — until
its game plugin identifies it (`SESSION_IDENTIFY`, `docs/v2/plugin-abi.md`).
A session that does not identify within 5 seconds of attaching stays hidden
indefinitely; core logs this once per session and never surfaces it through
API v1 unless the plugin eventually identifies it.

### `GET /api/v1/sessions`

Returns `{"sessions": [<summary>, ...]}`, every currently attached session, in
no particular order. `Cache-Control: no-store`.

### `GET /api/v1/sessions/{id|active}`

Returns one summary. 404 `unknown_session` if `id` does not currently exist, or
`active` addresses no focused session. `Cache-Control: no-store`.

### Session events: `WS /api/v1/sessions/subscribe`

Session add/remove/focus-change notifications reuse the field-subscription
protocol below ("Field subscriptions") applied to one core-maintained
document instead of a session's state: an object keyed by session id, each
value a session summary. A session's appearance, disappearance, and `focused`
flips are exactly the field-presence and field-value changes that protocol
already encodes, so no new frame shape is needed:

- A pointer such as `/<sessionId>` selects a whole summary; `/<sessionId>/focused`
  selects only its focus flag; the empty pointer `""` selects every session.
- A new session becoming visible is its pointer(s) newly present, with a
  value, in an update frame. A session ending is the *positions in this
  client's own request array* that selected it appearing in that frame's
  removed-index list — exactly 1.x's existing "became absent" mechanic, not a
  list of session ids. A focus change is a `focused` value flip on exactly the
  two affected sessions (the old and new focus), or a single flip if only one
  side is selected.
- Missing-but-valid pointers (an id not currently attached) report absence,
  exactly as in the base protocol; they do not invalidate the subscription.

Revisions on this document are independent of any session's own state
revision.

## Field subscriptions: `WS /api/v1/sessions/{id|active}/subscribe`

Derived directly from 1.x's `/websocket/v2/subscribe`
(`src/server/v2_subscription.rs`, README "Renderer state (v2)" and "WebSocket
API" sections), generalized from one fixed document to any session's merged
namespace document, and extended to replace the separate precise endpoint.
There is no `/api/v1/.../subscribe/precise`; a namespace's high-frequency
fields are sampled at their own rate only while a client has them selected,
exactly as the 1.x precise/regular reader lanes already activate on demand.
Namespace reader-lane declarations (which fields need faster sampling) are
`docs/v2/state-schema.md`'s concern; this protocol only consumes the resulting
`SubscriptionDemand`-shaped union across connected clients, as 1.x already
does.

### Addressing and document shape

The subscribed document is the selected session's merged state: a JSON object
keyed by namespace (`vosu.client`, `vosu.status`, `vosu.chart`, `vosu.play`,
`vosu.skin`, the game plugin's own namespace, any other installed plugin's
namespace), each value that namespace's own document. A pointer such as
`/vosu.play/combo` selects one field; `/vosu.play` selects a whole namespace;
`""` selects everything the session currently has.

Subscribing to `active` tracks the focused session rather than one fixed
session: if focus moves to a different session while subscribed, the next
frame is a fresh `[0, revision, ...]` initial frame built from the newly
focused session's document (its own baseline, not a diff against the
previous session's values), and the revision counter continues from the
hub's own sequence. This happens with no client message and does not close
the connection.

### Establishing a subscription

The endpoint expects one text message within 5 seconds of the upgrade: a JSON
array of 1–256 non-overlapping RFC 6901 JSON Pointers (each ≤512 bytes, ≤32
decoded tokens), exactly 1.x's limits. Any other content, or no message within
the deadline, fails the request. The selection is immutable: any later text
message receives `[2, 5, "subscription already established"]` and closes the
connection with code `1008`, identically to 1.x.

A pointer naming an unknown or not-yet-filled namespace or field is permitted
and reports absence (never present in an initial frame, never re-reported
unless it starts being filled); it does not invalidate the subscription. This
mirrors 1.x's "missing but syntactically valid pointers" rule exactly.

### Frames

Identical wire shapes to 1.x, addressed by position in the request array:

- Initial: `[0, revision, [[index, value], ...]]` — every currently present
  selected value.
- Update: `[1, revision, [[changedIndex, value], ...], [removedIndex, ...]]` —
  changed-and-present indices with their values, then indices that became
  absent.
- Error: `[2, code, message]`, always immediately followed by a close frame.
- Keepalive: WebSocket control frames only; vosu answers each Ping with a Pong.

Error codes reuse 1.x's numbering and add one, for addressing a session that
stopped existing between upgrade and the first subscribe message (`id`, not
`active`, which instead degrades to "no focused session" and reports absence
per namespace/field rather than erroring):

| Code | Meaning |
| --- | --- |
| 1 | invalid subscription request |
| 2 | invalid JSON Pointer |
| 3 | subscription timed out (no message within 5s) |
| 4 | state serialization failed |
| 5 | subscription already established |
| 6 | unknown session (`id` does not exist) |

### Backpressure and limits

Identical to 1.x: delivery is latest-only, never an append-only log; a newer
pending snapshot replaces an older unsent one for both data and control
frames. A connection whose pong queue is full closes with code `1011` rather
than pinning the reader. Frames and assembled messages are limited to 1 MiB,
matching the WebSocket API's general limit. Debounce applies per active
producing lane as 1.x's does; the configuration knob is a new
`API_V1_SUBSCRIPTION_DEBOUNCE` (default 1 ms), independent of
`V2_SUBSCRIPTION_DEBOUNCE`, since the two protocols run on separate hubs with
separate client populations.

## HTTP state reads

These are one-shot reads: they create no subscription demand and force no
fresh sample. They return whatever value is currently published — the same
value a concurrent subscriber would already have — at whatever cadence that
field's own lane already runs.

### Whole document and whole namespace

- `GET /api/v1/sessions/{id|active}/state` — the session's full merged
  document, same shape as the subscription baseline: `{"<namespace>": {...}, ...}`
  for every namespace the session currently fills.
- `GET /api/v1/sessions/{id|active}/state/{namespace}` — one namespace's whole
  document. 404 `unknown_namespace` if the session's plugins never fill it.

### JSON Pointer selection

Either route above accepts `?pointer=<RFC 6901 pointer>`, resolved against the
document that route would otherwise return, and responds `{"value": <json or
null>}`. A pointer that does not resolve (absent field, out-of-range index,
descent into a scalar) returns `{"value": null}`, the same observable result
as a field that resolves to an explicit JSON `null` — this matches the
subscription protocol's "absence" semantics and avoids a second channel for
presence. A pointer that fails to parse as RFC 6901 is 400 `invalid_pointer`.

Both routes are always `Cache-Control: no-store`: session state changes
continuously and these reads have no cache validator.

## Status codes and error bodies

Every API v1 error response is a JSON body:

```json
{ "error": { "code": "unknown_session", "message": "human-readable explanation" } }
```

| Status | `code` | Used by |
| --- | --- | --- |
| 400 | `invalid_request` / `invalid_pointer` / `invalid_arguments` | malformed query, body, or pointer |
| 401 | `unauthorized` | missing/invalid tool-session token or broker credential on an action route |
| 403 | `forbidden` | peer/Origin policy; or a valid tool session whose action id is not in its resolved `requires`/`optional` |
| 404 | `unknown_session` / `unknown_namespace` / `unknown_resource` / `unknown_tree` / `unknown_action` / `unknown_invocation` | id or path never valid for this server |
| 405 | `method_not_allowed` | wrong HTTP method on a route |
| 410 | `unpublished_resource` / `unpublished_tree` | a structurally valid, correctly signed id no longer in the published registry |
| 413 | `payload_too_large` | action-invocation body over the POST body limit |
| 500 | `serialization_failed` | internal encode failure |

The 404/410 split on resource and tree ids follows directly from how ids are
minted (below): verifying the HMAC is what proves an id was ever genuine. An
id that fails verification (wrong shape, wrong signature, unknown plugin
component) is indistinguishable from one that was never minted, so it is 404.
An id that verifies but is not in the current bounded registry was genuine and
has since rotated out (new publication, session ended, evicted for space); it
is 410, telling a long-lived consumer not to keep retrying it.

## Resources and trees

Exactly [ADR 0021](../decisions/0021-api-v1-and-upstream-adapters.md#resources):
files are resources the state refers to, never "current file" lookups.

- A **resource** is one file; a **tree** is an indexed folder (a chart's set
  folder, the current skin). A kind name is bare — one segment
  `[a-z0-9][a-z0-9-]{0,31}` — everywhere except inside a `resource:`
  capability id: core's chart kinds are `file`, `audio`, `background` (each a
  resource) and `set` (a tree); the skin tree has no further kind, just
  `skin`. Context already supplies the namespace (the enclosing
  `vosu.chart.resources` key, the route prefix, the plugin's own manifest
  section), so only the capability-id form is qualified:
  `resource:chart.background` for a core kind, `resource:<plugin-id>.<kind>`
  for a plugin's own.
- State carries references in place, never bare paths:
  `{"url": "/api/v1/resources/<id>", "type": "<mime>"}` or
  `{"url": "/api/v1/trees/<id>/"}`.

| Route | Serves | Cache-Control | ETag |
| --- | --- | --- | --- |
| `GET`/`HEAD /api/v1/resources/{id}` | one file; a single validated byte range | `public, max-age=31536000, immutable` | strong, content-derived |
| `GET /api/v1/trees/{id}/` | `{"entries":[{"path","size","type","hash"?}],"truncated":bool,"revision":string}` | `no-cache` (store, revalidate) | strong, revalidated |
| `GET`/`HEAD /api/v1/trees/{id}/{path}` | one file via the plugin's own lookup rules | `no-cache` | strong, revalidated |
| `GET /api/v1/sessions/{id\|active}/chart/{file\|audio\|background}`, `/api/v1/sessions/{id\|active}/skin/{path}` | `307` to the current resource URL | `no-store` | none |

`If-None-Match` (including weak comparisons, lists, and `*`) takes precedence
over `If-Modified-Since`; a satisfied condition returns a bodyless 304. HEAD
returns the same headers as GET with no body and ignores Range.

### Id registry

Core mints an id at **publish time**, directly from the plugin's reference
(`kind`, `key`, `name`, `content_id`) with no round trip to the plugin: an
HMAC under a persisted core secret of the plugin identity and that tuple. A
resource's type is assigned exactly once, at this same moment, from the
extension of `name` against the allowlist below — core never re-derives it
later, so the served `Content-Type` for that id is its published type for the
id's whole lifetime, even if the underlying file later changes. Each tree
entry is typed independently of its tree's own `name`, from that entry's own
`path` in the index core builds (below). `content_id` is the plugin's own
content identity (a content-store hash, or a path+size+modification-time
digest) and feeds the ETag. Ids are stable across restarts, unguessable, and
serve only from a bounded in-memory registry of currently published
references — there is no route to browse a game folder, and core never
serves a file that the current state or a published tree index did not
reference. Registry capacity and eviction policy belong to the core
implementation; this spec only fixes the observable 404/410 contract above.

Resolving an id to actual bytes is lazy: core asks the plugin to resolve
`{kind, key}` to a root+path or a stream only on that id's **first HTTP
request**, never at publish time, and caches the resolution for the
registry's lifetime of the id. Once core closes a stream-backed
resolution (`STREAM_CLOSE`, `docs/v2/plugin-abi.md`), it drops that cached
resolution; the next request re-resolves from scratch rather than reusing a
closed stream. Plugins resolve a key to either a declared root plus a
relative path — opened with
[ADR 0005](../decisions/0005-root-confined-file-serving.md)'s retained-handle
confinement — or a bounded byte stream for generated or `process`-plugin
content, which still supports the same single validated byte range as a
file-backed resource. Plugins never serve HTTP directly. Resources and trees
are read-only; changes go through actions.

### Headers and type allowlist

Every resource and tree response carries
`Content-Security-Policy: sandbox` and `X-Content-Type-Options: nosniff`, so a
page or SVG from a chart or skin folder runs without script and outside the
overlay origin even opened directly. API v1 owns this allowlist. A
resource's `Content-Type` is fixed at publish time (above) from its `name`'s
extension against it; each tree entry's `type` is derived the same way from
that entry's own `path` extension, independently of the tree reference's
`name` — never a plugin-chosen or path-embedded string:

| Family | Types |
| --- | --- |
| `image` | `image/png`, `image/jpeg`, `image/webp`, `image/gif` |
| `audio` | `audio/mpeg`, `audio/ogg`, `audio/wav`, `audio/flac` |
| `video` | `video/mp4`, `video/webm` |
| `chart-text` | `text/plain; charset=utf-8` |

A plugin's manifest only bounds which of these families each of its
resource/tree kinds may ever produce (`[[resource_kinds]]`,
`docs/v2/plugin-abi.md`, mirrored in `docs/v2/catalog-v2.md`'s catalog
entries); it never supplies a literal MIME type. A file extension outside the
bounded families, or outside this allowlist entirely, serves as
`application/octet-stream`.

## Actions

Tools invoke plugin actions. The capability id is `action:<plugin-id>.<name>`
(`requires`/`optional`, catalog entries), but routes and `vosu.invoke`
address the action by its bare `{plugin-id}.{name}` — the `action:` prefix
exists only in capability ids. Overlays never invoke actions (authorization
below). Each action declares, in its owning plugin's manifest (`[[actions]]`,
`docs/v2/catalog-v2.md`), whether it needs a session: `required`, `optional`,
or `none`.

### Invoke

- `POST /api/v1/sessions/{id|active}/actions/{plugin-id}.{name}` — for an
  action declared `required` or `optional`; the action receives this
  resolved session id.
- `POST /api/v1/actions/{plugin-id}.{name}` — for an action declared `none`
  or `optional`; the action receives no session (`session: null`).

Calling an action at the route its own `session` declaration does not allow
(`required` at the session-less route, `none` at a session route) is 404
`unknown_action`: it is simply not registered there.

Body: `{"arguments": {...}}`, matching the action's declared `args_schema`
(`docs/v2/catalog-v2.md`). Core waits up to 250 ms for the action's result:

- Reaches any terminal state (`succeeded`, `failed`, or `cancelled`) within
  that window: `200` with the same invocation object
  `GET /api/v1/invocations/{id}` would return (`state` plus `result`, if
  `succeeded`, or `error`, if `failed`).
- Still `running` after 250 ms: `202 {"invocationId": "<opaque string>"}`
  immediately, with the action continuing; use the invocation route below
  for its eventual outcome.

The action's own ABI result maps onto this API outcome (and onto the
invocation `state` below) as:

| ABI result | API outcome |
| --- | --- |
| success | `succeeded`, `result` = the action's value |
| failure, `code: "cancelled"` | `cancelled`, no `error` |
| failure, any other `code` | `failed`, `error = {code, message}` |

Two outcomes are core-synthesized rather than reported by the action itself:
a plugin action that exceeds its timeout is `failed` with
`code: "timed_out"`; a plugin host that crashes or is killed mid-invocation
is `failed` with `code: "plugin_crashed"`.

### Progress, result, and cancellation: `/api/v1/invocations/{invocationId}`

Invocation ids are server-wide, not nested under a session or its actions —
the originating session (or `null`) is fixed at invoke time and does not
change if that session later ends.

- `GET /api/v1/invocations/{invocationId}` polls current status:

  ```json
  {
    "state": "running",
    "progress": { "fraction": 0.42, "message": "string" },
    "result": null,
    "error": null
  }
  ```

  `state` is `running`, `succeeded`, `failed`, or `cancelled` (mapping above).
  `progress` is `null` when the action reports none. `result` is populated
  only once `succeeded`; `error` (the same `{code, message}` shape as HTTP
  errors) only once `failed`.
- `DELETE /api/v1/invocations/{invocationId}` requests cancellation. It is
  idempotent and advisory: the action may finish before the request is
  observed, in which case its final `succeeded`/`failed` state stands.
- An invocation id expires once its action reaches a terminal state and has
  been read once, or after an implementation-defined bound; a 404
  `unknown_invocation` after that is expected, not an error condition to
  guard against.

Invoke, poll, and cancel requests are capped at the existing 1 MiB POST body
limit.

### Authorization

Reads, resources, trees, and subscriptions need no authorization beyond the
existing peer policy (`ALLOWED_IPS`): open like other overlay-origin reads in
1.x. Actions are different — only three caller classes may invoke one, and an
overlay (browser script served from the overlay origin, with no further
credential) never can:

- **A tool UI**, through the browser bridge. The dashboard mints a tool UI's
  write-authority session exactly as [ADR 0017](../decisions/0017-tool-host.md)
  already does — a single-use launch code exchanged for a session held only
  in that tab's page memory — and the UI presents it on every action route as
  `X-Vosu-Tool-Session: <token>`. Core checks the token against the live
  session, then that action's capability id (`action:{plugin-id}.{name}`)
  must be present in the owning tool's resolved `requires`/`optional`
  (`docs/v2/catalog-v2.md`'s permission-surface rule: declaring the
  capability *is* declaring the intent to call it) —
  otherwise 403 `forbidden` even with a valid session. The bridge exposes
  `vosu.invoke(actionId, args, { session })` to tool-UI JavaScript: it issues
  `POST /api/v1/sessions/{session|active}/actions/{actionId}` when `session`
  is given or defaulted to `"active"`, or `POST /api/v1/actions/{actionId}`
  when `session: null` is passed, attaching that header automatically.
  `docs/v2/catalog-v2.md` references this section for `vosu.invoke` rather
  than restating it.
- **The CLI**, through the native loopback broker credential, for an action
  declared `cli = true` (`docs/v2/catalog-v2.md`): `vosux <plugin-id>
  <action> [json-args]` forwards to the same HTTP route under the broker's
  own authority, bypassing the tool-session check. A session-scoped action is
  invoked against `active` unless the CLI is given `--session <id>`.
- **A plugin**, calling another plugin's action through the ABI
  (`docs/v2/plugin-abi.md`) entirely inside the host process; this never
  reaches the HTTP surface at all.

A request to an action route without a valid tool-session token or broker
credential is 401 `unauthorized`. There is no API-key scope for actions:
`settings:write` and `profiles:manage` remain the only key scopes (existing
counter settings/profile writes, unrelated to API v1), and CORS does not
allow `X-Vosu-Tool-Session` or `Authorization` cross-origin, so no action
route is reachable from a page on another origin either way.

## Listener placement, CORS, and limits

API v1 is served entirely on the overlay listener, alongside the
upstream-compatible adapters; it never appears on the dashboard listener,
which rejects every overlay-origin route including this one
([ADR 0016](../decisions/0016-separate-dashboard-origin.md)). Peer policy
(`ALLOWED_IPS`), CORS (no `Authorization` in allowed headers, no cross-origin
credentialed reads), and request limits (1 MiB POST bodies, 1 MiB WebSocket
frames/messages) are the same settings 1.x already enforces for the overlay
listener; API v1 adds no new listener-level knob beyond
`API_V1_SUBSCRIPTION_DEBOUNCE` above.

## Route ownership: upstream-compatible adapters

These surfaces stay bug-for-bug compatible with the project they imitate
([ADR 0021](../decisions/0021-api-v1-and-upstream-adapters.md#upstream-adapters));
this spec defines none of their behavior, only that API v1 does not share
routes with them:

| Adapter | Routes |
| --- | --- |
| Upstream v2 (precise variant included) | `GET /json/v2`, `GET /json/v2/precise`, `WS /websocket/v2`, `WS /websocket/v2/precise`, `GET /files/...`, `GET /Songs/{path}`, `GET /backgroundImage`, `GET /api/calculate/pp`, `GET /api/ingame` |
| gosuMemory-compatible | `GET /json`, `WS /ws` |
| streamCompanion-compatible | `GET /json/sc`, `WS /tokens` |

`/websocket/v2/subscribe` is removed; API v1's field-subscription protocol
above replaces it for every first-party consumer
([ADR 0022](../decisions/0022-install-resolver-and-2-0-transition.md)).

## Worked examples

### Cross-game combo/accuracy overlay

An overlay that works for any installed game plugin selects only common-layer
fields:

```json
["/vosu.play/combo", "/vosu.play/accuracy", "/vosu.play/accuracyKind"]
```

sent as the first message to `WS /api/v1/sessions/active/subscribe`. It
renders `combo` directly and labels `accuracy` with `accuracyKind` (each
game's own formula name, per `docs/v2/state-schema.md`) rather than assuming
one formula. A plugin that does not fill `vosu.play.accuracy` for some mode
reports it absent, which the overlay should treat as "no accuracy to show",
not zero.

### Background image via a resource reference

```http
GET /api/v1/sessions/active/state/vosu.chart
```

returns a chart document whose `resources` object carries the current
background as a resource reference under its `background` key (alongside
`file`, `audio`, and the `set` tree reference, `docs/v2/state-schema.md`), for
example:

```json
{ "resources": { "background": { "url": "/api/v1/resources/8f2c…", "type": "image/webp" } } }
```

The overlay sets `resources.background.url` directly as an `<img>` `src` (or
fetches `GET /api/v1/sessions/active/chart/background` for the same content
without first reading state). The resource id is stable for that chart's
current background across restarts (it is minted from content identity, not
a session-scoped path), so the browser's own HTTP cache serves repeat loads
without a new request; a different chart publishes a different id rather
than reusing this URL for different content.

## Open questions

- **Action progress transport.** This spec uses plain polling GET for
  progress/result/cancellation. Pushing progress over the existing
  subscription channel (as a synthetic per-invocation field) would avoid
  polling latency for slow actions but adds a transient, non-schema-backed
  document to the subscription model. Recommendation: ship polling for v1;
  revisit only if a real action's UX needs sub-poll-interval progress.
- **Resource type family ownership.** The fixed `image`/`audio`/`video`/
  `chart-text` allowlist here is a core list; a plugin's manifest only bounds
  which of these family tokens each of its resource/tree kinds may produce
  (`docs/v2/plugin-abi.md`'s `[[resource_kinds]]`, mirrored in
  `docs/v2/catalog-v2.md`), and core assigns the actual MIME type by file
  extension. If a future kind genuinely needs a type outside these four
  families, that is an ADR 0021 change, not something this spec can resolve
  unilaterally.
- **Invocation id lifetime.** This spec fixes the observable behavior (404
  once expired) but leaves the exact retention bound to the implementation,
  mirroring how the resource registry's capacity is unspecified here.
  Recommendation: tie it to a short grace window after a terminal state is
  first read (invocation ids are no longer session-scoped, so they do not
  share a session's lifetime), so a client that reads the result once cannot
  leak invocations across a long-running daemon process.
