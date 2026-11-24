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

The rule ranges over identified sessions only (see Visibility below). An
identified session whose game plugin set `focus_priority: true` on its
`SESSION_IDENTIFY` (`docs/v2/plugin-abi.md` §6.2; core holds at most one
such session at a time — the first identified, kept until it detaches — and
ignores the flag on any later `SESSION_IDENTIFY` while one is already held)
is always the focused
session while it exists, regardless of OS foreground or recency. The osu!
plugin sets this on the stable tournament-manager session once the M4a-
or-later aggregate task (`docs/v2/implementation-plan.md`) lands, matching
1.x's `select()`, which prefers the tournament manager unconditionally
(`src/instance.rs:702-721`, `src/runtime/worker.rs:873-896`) — nothing
else in 2.0 sets it at this freeze (if two `--tournament` processes somehow
attach at once, 2.0 keeps whichever is identified first, which need not be
1.x's lowest-PID choice; an edge of an edge). Otherwise, exactly
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

Returns `{"sessions": [<summary>, ...]}`, every currently identified session, in
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

A3/R23: each identification, detach or focus transition publishes one coherent
session document (including both focus flips), never an intermediate two-focus
document. Session arrays have no ordering guarantee. Each subscription stream
has an initial revision and a monotonically increasing hub-owned sequence,
independent of session state revisions; rebinding `active` never resets it.

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
`""` selects everything the session currently has. A namespace's
`"x-visibility": "plugins"` subtree ([`docs/v2/state-schema.md`](state-schema.md),
D2 — only `vosu.osu.internal` in this freeze) is never present in this
document for any API v1 consumer: core's projector strips it before this
protocol, every HTTP state read below, every session-event frame, and every
debug log ever see the document, regardless of what pointer is selected.

Subscribing to `active` tracks the focused session rather than one fixed
session: if focus moves to a different session while subscribed, the next
frame is a fresh `[0, revision, ...]` initial frame built from the newly
focused session's document (its own baseline, not a diff against the
previous session's values), and the revision counter continues from the
hub's own sequence. This happens with no client message and does not close
the connection.

A3/R22: an initial `active` upgrade with no focused session is rejected with
HTTP 404 `unknown_session`, like an HTTP read. Once upgraded successfully, loss
of all focus reports selected values absent and keeps the connection subscribed;
the next focus emits a fresh initial frame. An established fixed-id subscription
whose session detaches sends a final update removing every previously present
selected index, then closes normally with code `1000`. It never follows another
session. A detach before establishment uses error 6, followed by close `1008`.

### Establishing a subscription

The endpoint expects one text message within 5 seconds of the upgrade: a JSON
array of 1–256 non-overlapping RFC 6901 JSON Pointers (each ≤512 bytes, ≤32
decoded tokens). These explicit bounds govern decoded-token overlap checking.
Any other content, or no message within the deadline, fails the request. The
selection is immutable: any later text
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
| 404 | `unknown_session` / `unknown_namespace` / `unknown_resource` / `unknown_tree` / `unknown_action` / `unknown_job` | id or path never valid for this server; `unknown_job` also for a job another tool session created, so its existence is not disclosed |
| 405 | `method_not_allowed` | wrong HTTP method on a route |
| 410 | `unpublished_resource` / `unpublished_tree` | a structurally valid, correctly signed id no longer in the published registry |
| 416 | `invalid_range` | malformed, multiple or unsatisfiable resource/tree-member byte range; includes `Content-Range: bytes */<size>` |
| 413 | `payload_too_large` | job creation/poll/cancel body over the POST body limit, or a job-creation body within that limit whose CBOR re-encoding would exceed the plugin ABI's 1 MiB frame cap (`docs/v2/plugin-abi.md` §6.8) |
| 429 | `too_many_jobs` | creating a job while 1024 jobs are still running |
| 500 | `serialization_failed` | internal encode failure |
| 502 | `provider_failed` | resource/tree provider failure, malformed reply, invalid indexed-member proof, or source open/read failure |
| 504 | `provider_timed_out` | resource/tree resolution, listing, lookup or stream-read deadline elapsed |

Resource/tree providers have a five-second deadline **per operation** (resolve,
list, lookup, and each stream-read reply), not for the whole download.

The 404/410 split on resource and tree ids follows directly from how ids are
minted (below): verifying the HMAC is what proves an id was ever genuine. An
id that fails verification (wrong shape, wrong signature, unknown plugin
component) is indistinguishable from one that was never minted, so it is 404.
An id that verifies but is not in the current bounded registry was genuine and
has since rotated out (new publication, session ended, evicted for space); it
is 410, telling a long-lived consumer not to keep retrying it.

Provider errors never disclose private root/source paths. A valid tree with no
matching indexed member returns 404 `unknown_resource`, not 502. Confirmed
immutable-source invalidation retires the id and returns 410, not changed bytes.
Provider failures are not `serialization_failed`; action failures after job
creation are represented in the job, not as resource-provider HTTP statuses.

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
| `GET`/`HEAD /api/v1/resources/{id}` | one file; a single validated byte range | `public, max-age=31536000, immutable` | strong, representation-identity-derived |
| `GET /api/v1/trees/{id}/` | `{"entries":[{"path","size","type","hash"?}],"truncated":bool,"revision":string}` | `no-cache` (store, revalidate) | strong, revalidated |
| `GET`/`HEAD /api/v1/trees/{id}/{path}` | one file via the plugin's own lookup rules | `no-cache` | strong, revalidated |
| `GET /api/v1/sessions/{id\|active}/chart/{file\|audio\|background}`, `/api/v1/sessions/{id\|active}/skin/{path}` | `307` to the current resource URL | `no-store` | none |

`If-None-Match` (including weak comparisons, lists, and `*`) takes precedence
over `If-Modified-Since`; a satisfied condition returns a bodyless 304. HEAD
returns the same headers as GET with no body and ignores Range.
The tree index route is GET only: HEAD returns 405 `method_not_allowed`.

### Id registry

Core mints an id at **publish time**, directly from the plugin's reference
(`kind`, `key`, `name`, `content_id`) with no round trip to the plugin: an
HMAC under a persisted core secret of the plugin identity and that tuple. A
resource's type is assigned exactly once, at this same moment, from the
extension of `name` against the allowlist below — core never re-derives it
later, so the served `Content-Type` for that id is its published type for the
id's whole lifetime, even if the underlying file later changes. Each tree
entry is typed independently of its tree's own `name`, from that entry's own
`path` in the validated index (below). `content_id` is the plugin's content
identity and feeds the ETag. Mutable assets use a disk-backed metadata identity,
prepared on a cold worker when source facts change, never on capture ticks.
The opened file's identity includes size and, on Unix, device/inode plus
nanosecond mtime and ctime; on Windows it includes volume serial, file id,
LastWriteTime and FILE_BASIC_INFO.ChangeTime. No representation bytes are retained
in RAM. Each disk stream re-verifies its opened handle immediately before reading
and after each bounded read, discarding the chunk and returning 410 on mismatch.
Changed identity requires a new publication and id; an old id serves only its
original representation or returns 410, never silently changed bytes. Unix ctime
detects in-place writes even if a user restores size and mtime. Residual risk:
a privileged Windows actor that resets ChangeTime as well as LastWriteTime can
defeat this metadata identity; it is not a cryptographic content hash. Ids are
stable across restarts, unguessable, and
serve only from a bounded in-memory registry of currently published
references — there is no route to browse a game folder, and core never
serves a file that the current state or a published tree index did not
reference. Registry capacity and eviction policy belong to the core
implementation; this spec only fixes the observable 404/410 contract above.

A3/R14/R17: token identity includes plugin identity, reference class and the
deterministically encoded `(kind, key, name, content_id)` tuple; never public
session id, PID, handle or instance generation. The canonical CBOR and duplicate
key rules are in [plugin-abi.md](plugin-abi.md) §6.5. Identical references can
have several live session owners; removing one owner does not retire the others.
Authenticity is verified before membership lookup. The token alone is not
authority to serve bytes.

Tree members require a validated cached index before lookup. A successful
`TREE_LOOKUP` includes `entry_path`, the canonical logical member in that index.
The request may be an alias/fallback, but `entry_path` must be indexed, and its
extension determines type policy. Entries omitted from a truncated index cannot
be served through it. Declared-root confinement remains an additional mandatory
check. Tree representations are revalidated, not immutable.

Resolving an id to actual bytes is lazy: core asks the plugin to resolve
`{kind, key}` to a root+path or a stream only on that id's **first HTTP
request**, never at publish time. Successful file resolutions may be cached;
failed opens are not cached as permanent failures. Stream resolutions are
request-owned leases: close at request end invalidates that resolution, the next
request resolves afresh, and no request can close another request's stream.
Plugins resolve a key to either a declared root plus a
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
(`requires`/`optional`, catalog entries), but routes and the bridge address
the action by its bare `{plugin-id}.{name}` — the `action:` prefix exists
only in capability ids. Overlays never invoke actions (authorization below).
Each action declares, in its owning plugin's manifest (`[[actions]]`,
`docs/v2/catalog-v2.md`), whether it needs a session: `required`, `optional`,
or `none`.

Invoking an action starts a **job** — API v1's term for it; the ABI keeps its
internal term "invocation" (`docs/v2/plugin-abi.md`).

### Create a job

- `POST /api/v1/sessions/{id|active}/actions/{plugin-id}.{name}` — for an
  action declared `required` or `optional`; the job's `session` is this
  resolved session id.
- `POST /api/v1/actions/{plugin-id}.{name}` — for an action declared `none`
  or `optional`; the job's `session` is `null`.

Calling an action at the route its own `session` declaration does not allow
(`required` at the session-less route, `none` at a session route) is 404
`unknown_action`: it is simply not registered there.

Body: `{"arguments": {...}}`, matching the action's declared `args_schema`
(`docs/v2/catalog-v2.md`). An optional `wait=<ms>` query parameter (0–30000,
default 250) bounds how long this request waits for the job:

- `wait=0` starts the job and responds immediately, without waiting at all:
  `202` with the job object (`state: "running"`) and
  `Location: /api/v1/jobs/{id}`.
- The job reaches any terminal state (`succeeded`, `failed`, or `cancelled`)
  within `wait`: `200` with the job object.
- Still `running` after `wait`: `202` with the job object
  (`state: "running"`) and `Location: /api/v1/jobs/{id}`, as for `wait=0`;
  use the job route below for its eventual outcome.

Arguments are validated against the bounded action-schema profile in
[catalog-v2.md](catalog-v2.md#bounded-action-schema-profile-a3) before job
creation or dispatch; failure is 400 `invalid_arguments`. Successful results
are validated before publication; invalid output terminates the job as
`failed` with `invalid_result`, without exposing raw sensitive values.
All action and job responses, including errors, use `Cache-Control: no-store`.

### Job object

```json
{
  "id": "string",
  "action": "{plugin-id}.{name}",
  "session": "string|null",
  "state": "running",
  "revision": 0,
  "progress": { "fraction": 0.42, "message": "string" },
  "result": null,
  "error": null
}
```

`state` is `running`, `succeeded`, `failed`, or `cancelled`. `revision` is a
`u64` that increases on every progress update or state change, so a
long-polling client can detect a change without diffing the whole object.
`progress` is `null` when the action reports none. `result` is populated only
once `succeeded`; `error` (the same `{code, message}` shape as HTTP errors)
only once `failed`.

The action's own ABI result maps onto `state` as:

| ABI result | `state` |
| --- | --- |
| success | `succeeded`, `result` = the action's value |
| failure, `code: "cancelled"` | `cancelled`, no `error` |
| failure, any other `code` | `failed`, `error = {code, message}` |

Two outcomes are core-synthesized rather than reported by the action itself:
a plugin action that exceeds its timeout is `failed` with
`code: "timed_out"`; a plugin host that crashes or is killed mid-invocation
is `failed` with `code: "plugin_crashed"`.

A3/ML: the core invocation policy defaults to a 30-second deadline, independent
of HTTP `wait`; tests inject the same policy with bounded overrides. There is
no per-action timeout manifest field. First accepted terminal result wins;
timeout is terminal even when an in-process callee continues, and late results
cannot resurrect a job. Cancellation is advisory, not a terminal transition.
M3 proves `plugin_crashed` through the production instance-loss lifecycle seam;
actual subprocess crash/kill and process cancellation grace are M5 evidence.

### `/api/v1/jobs/{id}`

Job ids are server-wide, not nested under a session or its actions — the
originating session (or `null`) is fixed at job creation and does not change
if that session later ends.
Detach requests cancellation of session-bound work; the terminal result or
deadline resolves it. An invocation never retargets a newly focused `active`.

- `GET /api/v1/jobs/{id}` returns the job immediately. With
  `after=<revision>&wait=<ms ≤ 30000>` it long-polls instead: the response
  waits until `revision` has advanced past `after`, the job reaches a
  terminal state, or `wait` elapses, then returns `200` with the job object
  either way — an elapsed `wait` with no change still returns the unchanged
  job, never an error. Core holds at most 8 waiting requests per job and 256
  in total; a request beyond those bounds returns immediately with the
  current job instead of waiting.
- `DELETE /api/v1/jobs/{id}` requests cancellation. It is idempotent and
  advisory: the job may reach a terminal state before the request is
  observed, in which case that state stands.
  DELETE returns `200` with the current job object, whether still running or
  already terminal; it does not wait for cancellation to finish.
- **Retention**: a terminal job stays readable for 5 minutes after it
  terminates, in a bounded registry of 1024 jobs (the oldest terminal job is
  evicted first once the registry is full); past that window, or once
  evicted, `GET`/`DELETE` on its id is 404 `unknown_job`. Running jobs count
  toward the bound and are never evicted: creating a job while 1024 are
  running is 429 `too_many_jobs`.
- **Authorization**: `GET` and `DELETE /api/v1/jobs/{id}` are action routes
  and need the same tool-session token or broker credential as creating a
  job (Authorization below). A tool session can read and cancel only jobs it
  created (another session's job is 404 `unknown_job`); the broker credential
  can read and cancel any job.
  Ownership means the exact live launch-session record, not tool id or another
  tab. Expired/revoked tokens lose access; jobs never transfer to a new launch.

Job creation, polling, and cancellation requests are capped at the existing
1 MiB POST body limit.

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
  otherwise 403 `forbidden` even with a valid session. The bridge exposes two
  calls, both attaching that header automatically: `vosu.invoke(actionId,
  args, { session, onProgress, signal })` awaits the job to completion via
  `GET /api/v1/jobs/{id}` long-polling and resolves with its `result`,
  rejects with its `error` when it fails, and rejects with an error whose
  `code` is `"cancelled"` when it is cancelled (including after the passed
  `signal` aborts, which sends `DELETE /api/v1/jobs/{id}`); it calls
  `onProgress(progress)` on each change. `vosu.start(actionId, args,
  { session })` creates the job with `wait=0` and returns a job handle
  `{id, done, onProgress(fn), cancel()}` for a caller that wants it to run in
  the background — `done` is a promise settling the same way `vosu.invoke`
  does, `onProgress` registers a callback, and `cancel()` sends `DELETE`.
  Both route by `session`: omitted or a string (a session id or `"active"`;
  omitted means `"active"`) uses `POST /api/v1/sessions/{session}/actions/…`,
  and `null` uses `POST /api/v1/actions/…`; the action's declared `session`
  mode decides which of the two is valid, as Create a job defines.
- **The CLI**, through the native loopback broker credential, for an action
  declared `cli = true` (`docs/v2/catalog-v2.md`): `vosux <plugin-id>
  <action> [json-args]` forwards to the same HTTP route under the broker's
  own authority, bypassing the tool-session check, and by default waits for
  the job and prints its progress (as `vosu.invoke` does); `--detach` instead
  creates it with `wait=0`, prints the job id, and exits (as `vosu.start`
  does). A session-scoped action is invoked against `active` unless the CLI
  is given `--session <id>`.
- **A plugin**, calling another plugin's action through the ABI
  (`docs/v2/plugin-abi.md`) entirely inside the host process; this never
  reaches the HTTP surface at all.

A3/MM/MN/R25: action credentials are classified before generic authorization
middleware rejects unrelated credentials. They never bypass peer/Origin
policy, widen read access, or grant an API-key action scope. Transitional v2
tool manifests resolve action grants only against loaded installed providers;
legacy permissions grant none. The private loopback broker descriptor lists
CLI-exposed action ids and session modes (not a new public discovery endpoint).
The CLI uses `active` for required/optional, the session-less route for none,
and rejects `--session` for none; no 404 probe-and-retry. Metadata is not
authority: the server rechecks `cli=true`, loopback credentials and live grants,
retaining no-proxy/no-redirect and loopback-origin enforcement.

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

The normative [adapter inventory](../../tests/fixtures/upstream/ADAPTER_CONTRACT.md)
enumerates removals separately for each endpoint, including nested tournament
copies. The regular v2 `keys` object is absent entirely; precise upstream
`k1`/`k2`/`m1`/`m2` and legitimate all-hit histories remain. Both removed exact
routes (`/websocket/v2/subscribe` and `/files/skin/`) return ordinary 404 without
upgrade or static fallthrough. Lazer legacy skin lookup is 404; API v1 skin
trees remain supported. Legacy calculation accepts only a relative child of
the selected session's declared songs root or an absolute spelling confined
inside that session's declared roots. The separate calculation engine/query
contract is unchanged.

Bundled capture and file authority follow [the permanent surface contract](adapter-surfaces.md).
An active-session change never rebinds an already selected file response.
Removed-route attribution is bounded, advisory and based on validated overlay
identity only; API v1 never discloses the private path grant used for echoes.

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

- **Resource type family ownership.** The fixed `image`/`audio`/`video`/
  `chart-text` allowlist here is a core list; a plugin's manifest only bounds
  which of these family tokens each of its resource/tree kinds may produce
  (`docs/v2/plugin-abi.md`'s `[[resource_kinds]]`, mirrored in
  `docs/v2/catalog-v2.md`), and core assigns the actual MIME type by file
  extension. If a future kind genuinely needs a type outside these four
  families, that is an ADR 0021 change, not something this spec can resolve
  unilaterally.
