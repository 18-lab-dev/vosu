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
opaque core-minted string; `active` addresses the focused session wherever a
route accepts `{id|active}`. Addressing `active` when no session is focused
behaves as an unknown id (see status codes below).

A session summary is:

```json
{
  "id": "string",
  "game": "string",
  "variant": "string",
  "name": "string",
  "version": "string",
  "pid": 0,
  "focused": true,
  "plugin": { "id": "string", "version": "semver string" }
}
```

These are exactly the `vosu.client` fields from the naming contract, plus the
session `id` as the record's own key/identity.

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
- A new session becoming visible is the corresponding id's pointer(s) newly
  present in an update frame. A session ending is the corresponding id
  appearing in that frame's removed-ids list. A focus change is a `focused`
  value flip on exactly the two affected sessions (the old and new focus), or
  a single flip if only one side is selected.
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
| 401 | `unauthorized` | missing/invalid API key on a scoped route |
| 403 | `forbidden` | peer/Origin policy, or a valid key missing the required scope |
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
  folder, the current skin). Core-defined kinds are `chart.file`,
  `chart.audio`, `chart.background`, `chart.set`, `skin`; a plugin's own kinds
  are `resource:<plugin-id>.<kind>`.
- State carries references in place, never bare paths:
  `{"url": "/api/v1/resources/<id>", "type": "<mime>"}` or
  `{"url": "/api/v1/trees/<id>/"}`.

| Route | Serves | Cache-Control | ETag |
| --- | --- | --- | --- |
| `GET`/`HEAD /api/v1/resources/{id}` | one file; a single validated byte range | `public, max-age=31536000, immutable` | strong, content-derived |
| `GET /api/v1/trees/{id}/` | index of `{path, size, hash?}` entries, `truncated`, `revision` | `no-cache` (store, revalidate) | strong, revalidated |
| `GET`/`HEAD /api/v1/trees/{id}/{path}` | one file via the plugin's own lookup rules | `no-cache` | strong, revalidated |
| `GET /api/v1/sessions/{id\|active}/chart/{kind}`, `/api/v1/sessions/{id\|active}/skin/{path}` | `307` to the current resource URL | `no-store` | none |

`If-None-Match` (including weak comparisons, lists, and `*`) takes precedence
over `If-Modified-Since`; a satisfied condition returns a bodyless 304. HEAD
returns the same headers as GET with no body and ignores Range.

### Id registry

Core mints an id when a plugin publishes a reference: an HMAC, under a
persisted core secret, of the plugin identity, the plugin's own key for the
content, and a content identity (a store hash, or path+size+modification
time). Ids are stable across restarts, unguessable, and serve only from a
bounded in-memory registry of currently published references — there is no
route to browse a game folder, and core never serves a file that the current
state or a published tree index did not reference. Registry capacity and
eviction policy belong to the core implementation; this spec only fixes the
observable 404/410 contract above.

Plugins resolve a key to either a declared root plus a relative path — opened
with [ADR 0005](../decisions/0005-root-confined-file-serving.md)'s
retained-handle confinement — or a bounded byte stream for generated or
`process`-plugin content. Plugins never serve HTTP directly. Resources and
trees are read-only; changes go through actions.

### Headers and type allowlist

Every resource and tree response carries
`Content-Security-Policy: sandbox` and `X-Content-Type-Options: nosniff`, so a
page or SVG from a chart or skin folder runs without script and outside the
overlay origin even opened directly. The `Content-Type` comes from a fixed
core allowlist, never a plugin-chosen string:

| Family | Types |
| --- | --- |
| Image | `image/png`, `image/jpeg`, `image/webp`, `image/gif` |
| Audio | `audio/mpeg`, `audio/ogg`, `audio/wav`, `audio/flac` |
| Video | `video/mp4`, `video/webm` |
| Chart text | `text/plain; charset=utf-8` |

A plugin declares which family each of its resource/tree kinds belongs to in
its manifest (`docs/v2/catalog-v2.md`); it cannot introduce a new MIME type.
Anything the allowlist does not cover, including HTML, serves as
`application/octet-stream`.

## Actions

Tools and overlays invoke plugin actions named `action:<plugin-id>.<name>`.

### Invoke

`POST /api/v1/sessions/{id|active}/actions/{plugin-id}.{name}` with body
`{"arguments": {...}}` matching that action's declared argument schema
(`docs/v2/catalog-v2.md`). Two outcomes:

- The action completes within the request: `200 {"result": {...}}`.
- The action is long-running: `202 {"invocationId": "<opaque string>"}`.

### Progress, result, and cancellation

- `GET /api/v1/sessions/{id|active}/actions/{invocationId}` polls current
  status:

  ```json
  {
    "state": "running",
    "progress": { "fraction": 0.42, "message": "string" },
    "result": null,
    "error": null
  }
  ```

  `state` is `running`, `succeeded`, `failed`, or `cancelled`. `progress` is
  `null` when the action reports none. `result` is populated only once
  `succeeded`; `error` (the same `{code, message}` shape as HTTP errors) only
  once `failed`.
- `DELETE /api/v1/sessions/{id|active}/actions/{invocationId}` requests
  cancellation. It is idempotent and advisory: the action may finish before
  the request is observed, in which case its final `succeeded`/`failed` state
  stands.
- Invocation ids are scoped to the session and expire once their action
  reaches a terminal state and has been read once, or after an
  implementation-defined bound; a 404 `unknown_invocation` after that is
  expected, not an error condition to guard against.

Invoke, poll, and cancel requests are capped at the existing 1 MiB POST body
limit.

### Authorization

Reads, resources, trees, and subscriptions need no scope: they are open like
other overlay-origin reads in 1.x, subject only to the peer policy
(`ALLOWED_IPS`) that already gates every remote request. Actions mutate or
drive the attached client, so invoking one needs one of:

- A **dashboard-minted tool UI session**
  ([ADR 0017](../decisions/0017-tool-host.md)'s write-authority model):
  a tool UI opened from the dashboard holds a session kept only in page
  memory, which authorizes action invocation the same way it already
  authorizes a tool's own write routes.
- An **API key** ([ADR 0015](../decisions/0015-user-managed-api-keys.md)) with
  scope `actions:invoke`, for native and remote programmatic callers. This is
  a new scope alongside the existing `settings:write` and `profiles:manage`;
  it authorizes every `action:*` invocation and carries no per-action
  granularity in v1.
- The native loopback broker credential, which already bypasses scope checks
  for trusted local tooling.

A request without one of these to an action route is 401 (no/invalid key) or
403 (valid key, wrong scope; or dashboard-origin provenance missing CSRF).
CORS does not allow `Authorization`, so cross-origin browser callers cannot
use API keys, matching the existing 1.x policy.

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
["/vosu.play/combo", "/vosu.play/accuracy", "/vosu.play/accuracy_kind"]
```

sent as the first message to `WS /api/v1/sessions/active/subscribe`. It
renders `combo` directly and labels `accuracy` with `accuracy_kind` (each
game's own formula name, per `docs/v2/state-schema.md`) rather than assuming
one formula. A plugin that does not fill `vosu.play.accuracy` for some mode
reports it absent, which the overlay should treat as "no accuracy to show",
not zero.

### Background image via a resource reference

```http
GET /api/v1/sessions/active/state/vosu.chart
```

returns a chart document whose `background` field is a reference object, for
example:

```json
{ "background": { "url": "/api/v1/resources/8f2c…", "type": "image/webp" } }
```

The overlay sets that `url` directly as an `<img>` `src`. The resource id is
stable for that chart's current background across restarts (it is minted from
content identity, not a session-scoped path), so the browser's own HTTP cache
serves repeat loads without a new request; a different chart publishes a
different id rather than reusing this URL for different content.

## Open questions

- **Action progress transport.** This spec uses plain polling GET for
  progress/result/cancellation. Pushing progress over the existing
  subscription channel (as a synthetic per-invocation field) would avoid
  polling latency for slow actions but adds a transient, non-schema-backed
  document to the subscription model. Recommendation: ship polling for v1;
  revisit only if a real action's UX needs sub-poll-interval progress.
- **Per-action scope granularity.** `actions:invoke` is blanket: any key with
  it can invoke any action a session's plugins expose, including file-system
  or network side effects a specific action might have. Recommendation: keep
  one scope for v1 (matches `settings:write`'s existing breadth), and let the
  action's own declared trust/capability data in `docs/v2/catalog-v2.md`
  carry finer authorization if a concrete action needs it, rather than
  speculatively adding scopes with no current consumer.
- **Resource type family ownership.** The fixed image/audio/video/chart-text
  allowlist here is a core list; `docs/v2/catalog-v2.md` must describe how a
  plugin's manifest selects one of these families per resource/tree kind. If
  a future kind genuinely needs a type outside these four families, that is
  an ADR 0021 change, not something this spec can resolve unilaterally.
- **Invocation id lifetime.** This spec fixes the observable behavior (404
  once expired) but leaves the exact retention bound to the implementation,
  mirroring how the resource registry's capacity is unspecified here.
  Recommendation: tie it to the owning session's lifetime plus a short grace
  window after a terminal state is first read, so a client that reads the
  result once cannot leak invocations across a long-running daemon process.
