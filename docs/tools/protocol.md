---
type: Contract
title: "Tool protocol (2.0)"
description: "Package, routes, launch authority, API v1 state and the browser bridge between vosu 2.0 and web tools."
---

# Tool protocol (2.0)

A **tool** is a static web application packaged as a `kind = "tool"`
[`manifest.toml`](../v2/catalog-v2.md#kind--tool) package. It has no process,
service, port, token or command-line entry of its own: vosu serves its files,
and the tool reads game state and calls plugin actions through
[API v1](../v2/api-v1.md) from the browser. Work that needs native code,
filesystem access or a game client belongs in a [plugin](../plugins/authoring.md)
that declares an action; a tool reaches native work only through that action
([ADR 0019](../decisions/0019-plugin-runtime-and-trust.md)).

The 2.0 host cutover follows the
[A7 inventory](../v2/implementation-plan.md#route-method-listener-and-provider-inventory).
For the native/browser fixture receipts see [M7 X0](../testing/verification.md#m7-x0-functional-evidence).

- **Core** (the tool registry) discovers packages, keeps each tool's enabled,
  trust and settings state, resolves its `requires`/`optional` against the
  installed providers, mints launch codes and launch sessions, and authorizes
  every action and job request.
- **The tool host** (the bundled `vosu.toolhost`) serves each tool's static
  UI, art and public settings, the bridge, the theme and the design reference.
  It never mints or checks a session.
- **The dashboard** lists, enables, trusts, configures and opens tools.
  It reads only public API v1 summaries on the overlay origin; it never
  serves or proxies API v1. A settings write goes to core, not the web host.

The decision records are [ADR 0017](../decisions/0017-tool-host.md) (with its
2026-11-24 amendment) and ADR 0019. The maintained example is
[`examples/tools/hello-web`](../../examples/tools/hello-web/); the guided path
is [Create a tool](create-a-tool.md), and the [authoring reference](authoring.md)
and [design system](design.md) cover the rest.

## Package

A tool package is a directory (or a ZIP of it) with `manifest.toml` at its
root:

```toml
manifest_version = 1
kind = "tool"
id = "hello-web"                  # ^[a-z0-9][a-z0-9-]{0,31}$, not starting with "_"
name = "Hello web"
version = "0.2.0"                 # semver
description = "…"                 # optional
author = "18-LAB"                 # optional
homepage = "https://…"            # optional
icon = "web/icon.svg"             # optional, inside the package
banner = "web/banner.svg"         # optional, inside the package
host = ">=2.0.0-alpha.1, <3.0.0"  # semver requirement on the running vosu

[ui]                              # required
root = "web"                      # static directory served at /tools/<id>/
entry = "index.html"              # default "index.html"

[[requires]]                      # capabilities the tool cannot work without
id = "field:vosu.play"
version = "^1.0.0"

[[optional]]                      # capabilities it uses when available
id = "action:vosu.osu.beatmap.import"
version = "^1.0.0"
```

The shared fields, their validation and the capability grammar are the
[catalog v2 package manifest](../v2/catalog-v2.md#package-manifest-v2-manifesttoml).
A tool manifest has no `provides`, `runtime`, `[service]`, `[cli]`, `[api]` or
`[permissions]`; any of them makes it invalid. `requires` and `optional` are
the tool's **permission surface**: the install plan shows them, an
unresolvable `requires` entry blocks the tool, and an `action:` id grants an
action only when it is listed there **and** resolves to an installed provider.

`settings.json`, when present, uses the
[overlay settings schema](../../README.md#overlay-settings-profiles). The
dashboard edits it from the tool's card; profiles do not apply to tools.
`password` settings are write-only: the dashboard never shows them, a blank
field keeps the stored value, **Clear** removes it, and no tool route returns
them. A web tool therefore cannot read a `password` setting at all.

`icon` is square (SVG, or PNG at least 256×256) and is shown at 40–64 px.
`banner` is 3:1 (recommended 1200×400), cropped with `object-fit: cover`, so
keep its subject centred. A missing icon falls back to the name's initials.

A directory with only the 1.x `tool.toml` is never parsed, run or served. It
is listed as `disabled` with `status_detail: "needs-2.0-update"`, enabling,
trusting or launching it is refused with 400, and its files, settings and data
stay untouched for [the one-time 1.x migration](../migration/2.0.md#what-the-stable-first-start-migration-transfers).

## Discovery and state

Core scans, in precedence order:

| Root | Location | Trust |
| --- | --- | --- |
| Development | Each `vosu --tool <directory>` | Trusted for that run; replaces an installed tool with the same `id` |
| Installed | `<vosu data directory>/tools/<id>/` | Official catalog packages are trusted; others are untrusted until the user trusts them on the Tools page |

Roots are watched: an added, removed or edited `manifest.toml` or
`settings.json` applies without a restart. A tool is **servable** (ToolView
`status: "running"`) when it is valid, enabled, trusted and every `requires`
entry resolves. Disabling, untrusting, uninstalling or replacing a tool, or a
daemon restart, ends every launch session and pending code of that tool and
requests cancellation of their running jobs. A session never pins a provider:
each action request resolves the current one.

Installation, update and removal are catalog v2 install plans; see
[Catalog sources and installation](#catalog-sources-and-installation).

## 2.0 transition inventory (A7.2)

Frozen 2026-11-24 (A7.2) for M7; the full route, owner and deletion inventory
is the [A7 freeze](../v2/implementation-plan.md#a7-approved-contract-freeze-2026-11-24).
Each route is matched by exact method and path, or as the one `/tools/<id>`
prefix the tool host registers per presented tool. No prefix decides whether a
route survives; every other `/tools/…` method and path is 404, with no upgrade
and no static fallthrough.

### Overlay listener

| Method and path | Owner | Contract |
| --- | --- | --- |
| `GET /tools` | core | 302 to the dashboard Tools page; 404 when the dashboard is off |
| `POST /tools/_host/v1/session` | core | The [session exchange](#launch-and-session-exchange) |
| `GET /tools/_host/v2/bridge.js` | tool host | The [bridge](#browser-bridge), `text/javascript; charset=utf-8` |
| `GET /tools/_host/v1/theme.css` | tool host | `tokens.css` + `\n` + `system.css`, `text/css; charset=utf-8` ([design system](design.md)) |
| `GET /tools/_host/v1/design` | tool host | `design.html`, every component in both modes, `text/html; charset=utf-8` |
| `GET /tools/<id>` | tool host | 302 to `/tools/<id>/` |
| `GET`, `HEAD /tools/<id>/`, `/tools/<id>/<path>` | tool host | The package's `[ui].root`, root-confined: `/tools/<id>/` serves `entry`, `<dir>/` serves `<dir>/index.html`, and `<dir>` with an `index.html` is 302 to `<dir>/`. `Content-Type` from the extension, `Content-Length`, `X-Content-Type-Options: nosniff`. Traversal, a backslash, NUL or malformed percent-encoding is 404. 503 while the tool is not servable |
| `GET`, `HEAD /tools/<id>/_icon`, `/tools/<id>/_banner` | tool host | The declared art in every tool state, with `Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; img-src data:; sandbox`; 404 when not declared |
| `GET /tools/<id>/_settings` | tool host | `{"values":{…}}`: effective `settings.json` values with defaults applied, `password` settings omitted; `{}` without `settings.json`; 503 while not servable |

`/tools/<id>/api/<anything>` is 404 for every method, including WebSocket
upgrades, even when the package contains an `api` directory. Unknown tool ids
are 404. The `tools` overlay folder name is reserved.

These static, bridge and read routes remain available with the dashboard
off, subject to tool eligibility. No new tab can obtain a launch code
through dashboard management while off. The `/tools/_host/v1/session`
spelling is the retained exchange, not a v1 browser bridge or a service API.

### Dashboard listener (core)

Every `POST` requires dashboard provenance and `X-Vosu-CSRF`.

| Method and path | Contract |
| --- | --- |
| `GET /api/tools`, `GET /api/tools/<id>` | `{"tools":[ToolView]}` and one `ToolView` (below) |
| `POST /api/tools/<id>/enable`, `…/disable`, `…/trust` | State changes; a legacy package is 400 `needs-2.0-update` |
| `POST /api/tools/<id>/launch` `{"theme":"dark"\|"light"}` | `{"url"}`: see [launch](#launch-and-session-exchange) |
| `GET`, `POST /api/tools/<id>/settings` | GET `{"settings","values","secrets"}` with every `password` value `""` and `secrets: {"<id>": true\|false}`; POST `[{"uniqueID","value"}]` keeps a stored secret for `""` or an omitted field and clears it for `null` |

`ToolView`:

```json
{
  "id": "hello-web", "name": "Hello web", "version": "0.2.0",
  "description": "…", "author": "18-LAB", "homepage": null,
  "icon_url": "http://127.0.0.1:24050/tools/hello-web/_icon",
  "banner_url": "http://127.0.0.1:24050/tools/hello-web/_banner",
  "source": "official", "update_available": false,
  "trusted": true, "enabled": true, "status": "running", "status_detail": null,
  "ui_url": "http://127.0.0.1:24050/tools/hello-web/",
  "requires": [{"id": "field:vosu.play", "version": "^1.0.0", "satisfied": true, "provider": null}],
  "optional": [{"id": "action:vosu.osu.beatmap.import", "version": "^1.0.0", "satisfied": true, "provider": "vosu.osu"}],
  "has_settings": true
}
```

`source` is `dev`, `official` or `user`. `status` is `running` (servable),
`blocked` (a `requires` entry does not resolve), `untrusted`, `disabled` or
`invalid`; `status_detail` explains `invalid` and carries `needs-2.0-update`
for a legacy package. `provider` is the resolved plugin id, or `null` for a
built-in capability or an unresolved one.

### Deleted (404)

`GET /tools/_host/v1/{self,settings,context,overlays}`;
`GET /tools/_host/v1/bridge.js` and `assets/server/tool-bridge-v1.js`;
`GET /tools/_host/v1/cli`, `POST /tools/_host/v1/cli/<id>`,
`POST /tools/_host/v1/cli/<id>/open`, `PUT /tools/_host/v1/cli/<id>/autostart`
and `DELETE /tools/_host/v1/cli/session/<session>`; every method on
`/tools/<id>/api/<rest>`, including WebSocket upgrades;
`POST /api/tools/<id>/{start,stop,restart,open-data,clear-cache}`,
`GET /api/tools/<id>/logs` and `PUT /api/tools/<id>/autostart`; the
`/assets/tool-bridge.js`, `/assets/tool-bridge-v1.js` and
`/assets/design.html` copies. The `tool.toml` loader and its fallback,
services, readiness, ports, service and CLI tokens, `VOSU_TOOL_*`
environment, `tool.schema.json`, the vosux tool form and its `--list`,
`--json`, `--open` and `--autostart` options, and the bridge's `vosu.fetch`
are deleted too.

A tool has no replacement for the overlay list. A plugin that needs it calls
the sessionless `vosu.overlayhost.overlays.query` action, which refuses tool
and command-line callers
([catalog v2 "Overlay query action"](../v2/catalog-v2.md#overlay-query-action)).

The removed `/websocket/v2/subscribe`, `/files/skin/` index and
`/api/ingame` are ordinary 404s, not tool context or state routes. Use
overlay-origin API v1 for public state and resources. The dashboard notice
for `/api/ingame` offers the normal vosulay install plan. See the
[removed-route guide](../v2/migration-from-1x.md#overlays-that-used-removed-apis).

## Launch and session exchange

Write authority belongs to one browser tab, never to a URL, a cookie or the
tool's id. The exact rules are [API v1 "Tool launch sessions"](../v2/api-v1.md#tool-launch-sessions-a72):

1. **Launch.** The dashboard's **Open** calls `POST /api/tools/<id>/launch`
   and opens the returned
   `<overlay origin>/tools/<id>/#vosu-launch=<code>&theme=<dark|light>` in a
   new tab without an opener. Only a servable tool launches. The code is 256
   random bits, single-use and valid for 60 seconds; at most 1,024 unexpired
   codes exist, beyond which launch fails with 400.
2. **Exchange.** `POST /tools/_host/v1/session` on the overlay listener only,
   body `{"tool","code"}` (the 1 MiB POST limit applies). The code is compared
   in constant time and consumed whether or not the rest succeeds.

   | Status | Meaning |
   | --- | --- |
   | 200 `{"session"}` | A fresh 256-bit token; `Cache-Control: no-store` |
   | 400 | Invalid JSON, or `tool` or `code` missing |
   | 403 | Unknown, expired, reused or other-tool code, or the tool is no longer servable |
   | 429 | 1,024 live sessions already exist |

   No cookie is set and no query parameter is read.
3. **Holding.** The bridge removes the fragment with `history.replaceState`
   synchronously while the module evaluates, before any asynchronous work,
   and keeps the token only in its module memory.
4. **Scope.** The token is sent only as `X-Vosu-Tool-Session` on
   `POST /api/v1/actions/{action}`, `POST /api/v1/sessions/{session}/actions/{action}`
   and `GET`/`DELETE /api/v1/jobs/{id}` (and `GET`/`HEAD` of a job's
   artifacts). A launch session can access only the jobs it created. It
   does not grant authority on unrelated routes or change public read access.
5. **Lifetime.** A session lasts until the tool is disabled, untrusted,
   uninstalled or replaced, or vosu restarts. A reload, a bookmark or a
   replayed launch URL has no session and is read-only.

A tool UI must not copy the session into `localStorage`, `sessionStorage`,
IndexedDB, cookies or a URL, must not open same-origin pages with an opener,
and must not frame overlays: every overlay that later loads in the tab shares
that origin and its storage.

## Browser bridge

`GET /tools/_host/v2/bridge.js` is the one bridge: an ES module whose only
export is `vosu`. The tool host prepends the installed tools' identities and
the dashboard origin to `assets/server/tool-bridge.js`. Import it by that
absolute path; with a bundler, mark it external.

```js
import { vosu } from '/tools/_host/v2/bridge.js';
```

**Frozen type surface.** F T0 (vosu-tools) codes against exactly this; it
changes only through a later approved amendment. The job artifact members
(`Artifact`, `JobHandle.artifacts`, `vosu.artifacts`) are the AF.12 amendment
implemented by B0 ([Job artifacts](../v2/api-v1.md#job-artifacts-af12)).

```ts
type SessionTarget = string;              // a session id or "active"; nonempty, not "." or ".."
type ActionTarget = SessionTarget | null; // null: the session-less action route
type Progress = { fraction?: number; message?: string } | null; // API v1 job progress
type SettingValue = string | number | boolean | null | unknown[] | Record<string, unknown>;

interface ActionError extends Error {
  code: string; // "cancelled", "forbidden" (no launch session), an API v1
                // error code ("unauthorized", "forbidden", "invalid_arguments",
                // "unknown_action", "unknown_session", "timed_out",
                // "invalid_range", "unknown_job", "unknown_artifact",
                // "released_artifact", "too_many_downloads", "plugin_crashed", …),
                // or "http_error" / "invalid_job" when the response has none
}

interface ActionOptions {
  session?: ActionTarget;      // default "active"
  signal?: AbortSignal;        // abort before POST: no request; during POST: the
                               // created job is cancelled by id; while polling:
                               // DELETE /api/v1/jobs/{id}; after completion: nothing
  onProgress?: (progress: Progress) => void;
}

interface ArtifactReadOptions {
  method?: "GET" | "HEAD";                  // default "GET"; HEAD never opens the provider
  range?: { start: number; end?: number };  // one inclusive byte range: 206, or 416
                                            // "invalid_range" outside the artifact
  signal?: AbortSignal;                     // aborts the request; rejects "cancelled"
}

interface Artifact {
  readonly id: string;    // opaque; valid only under its own job
  readonly name: string;  // one file-name component, the attachment name
  readonly type: string;  // the served Content-Type
  readonly size: number;  // exact byte length
  fetch(options?: ArtifactReadOptions): Promise<Response>; // 200 or 206, body unread
  download(options?: { signal?: AbortSignal }): Promise<void>; // save as `name`
}

interface JobHandle {
  readonly id: string;
  readonly done: Promise<unknown>;  // resolves with result; rejects with ActionError
  cancel(): Promise<unknown>;       // advisory DELETE; a completed result stands
  onProgress(fn: (progress: Progress) => void): () => boolean; // ≤ 256 per handle
  artifacts(): Promise<readonly Artifact[]>; // after `done`; rejects as `done` does
}

type Status = "connecting" | "open" | "closed";

declare const vosu: Readonly<{
  ready: Promise<void>;           // exchanges the fragment code; rejects with Error
                                  // when the code is expired or reused
  tool: Readonly<{ id: string; name: string; version: string }>;
  dashboardUrl: string | null;    // dashboard origin (no trailing slash); null
                                  // when the dashboard is off (A7.3)
  colorScheme: "dark" | "light";  // launch `theme=`, else prefers-color-scheme;
                                  // applied once as <html data-theme>
  readonly canWrite: boolean;     // true only while this page holds a session
  settings(): Promise<Record<string, SettingValue>>;  // GET /tools/<id>/_settings
  subscribe(
    fields: string[],             // JSON Pointers, sent on every (re)connect
    onFrame: (frame: unknown) => void,
    onStatus?: (status: Status) => void,
    options?: { session?: SessionTarget },  // default "active"; null is a TypeError
  ): () => void;                  // stop: closes the socket, cancels reconnects
  invoke(actionId: string, args?: unknown, options?: ActionOptions): Promise<unknown>;
  start(actionId: string, args?: unknown, options?: ActionOptions): Promise<JobHandle>;
  artifacts(jobId: string, options?: { signal?: AbortSignal }): Promise<readonly Artifact[]>;
}>;
export { vosu };
```

Behavior frozen with the types:

- **Launch.** A `vosu-launch` fragment is removed from the URL and history
  entry before the module finishes evaluating, whether or not the exchange
  later succeeds; `theme` goes with it. Without the fragment nothing is
  exchanged and `ready` resolves at once. A refused code rejects `ready`, and
  every later `start`/`invoke` rejects with that error before any request.
- **Actions.** `actionId` must match `^[a-z0-9][a-z0-9._-]*$`, and a session
  target must be a nonempty string other than `.` and `..`; otherwise
  `start`/`invoke` reject with `TypeError` before any request. Without a
  session they reject with `code: "forbidden"` and send nothing. `start` posts
  `{"arguments": args}` with `wait=0` to
  `/api/v1/sessions/{session}/actions/{actionId}`, or to
  `/api/v1/actions/{actionId}` for `session: null`, then long-polls
  `GET /api/v1/jobs/{id}?after=<revision>&wait=30000`. The caller's signal is
  never passed to the POST, so an abort that races creation still learns the
  job id and cancels it. Progress callbacks run once per changed progress
  value, in microtasks; a late `onProgress` receives the current value.
- **Subscriptions.** `subscribe` keeps at most one WebSocket per call to
  `/api/v1/sessions/{session}/subscribe` on the page's own origin and sends
  `fields` on every (re)connect. It reconnects with capped exponential backoff
  (0.5 s doubling to 30 s, jittered between half and all of the step, reset
  only after 10 s open), retries at once on the browser `online` event, and
  ignores late events from a replaced socket. The returned function closes the
  socket and cancels any pending reconnect for good; calling it again does
  nothing. `onFrame` receives each parsed frame; a frame that is not JSON is
  skipped. Unknown option keys, a non-object option and an invalid session
  throw synchronously without opening a socket.
- **Artifacts.** A succeeded job's `artifacts` list (API v1
  [Job artifacts](../v2/api-v1.md#job-artifacts-af12)) becomes frozen
  `Artifact` objects. `handle.artifacts()` waits for `done` and reads the
  terminal job document `start` already polled, with no further request;
  `vosu.artifacts(jobId)` reads `GET /api/v1/jobs/{jobId}` once for a job this
  launch session created and resolves `[]` while it runs or when it ended
  other than `succeeded`. `invoke` still resolves with `result` alone: run an
  action that declares artifacts with `start`. Each artifact's URL is built
  from its job and artifact ids (nonempty, not `.`/`..`, percent-encoded); the
  listed `url` is never followed. `fetch` sends `GET` or `HEAD` with at most
  one `Range: bytes=start-[end]` and resolves with the unread `Response`, so
  a caller streams `body` or adapts it for media; any other status rejects
  with the API v1 error code. `download` saves the bytes as `name`: where
  `showSaveFilePicker` exists it opens the picker first (still inside the
  caller's click), then pipes the response body into the chosen file, so a
  multi-gigabyte artifact is never held in memory; otherwise it reads a
  `Blob` and clicks a temporary `<a download>` whose object URL is revoked
  after 30 seconds. Closing the picker rejects with `code: "cancelled"`.
  Without a launch session these methods reject with `code: "forbidden"` and
  send nothing; another tab's session gets 404 `unknown_job`, and a revoked
  session cannot read what it created.
- **Requests.** Every request is to the page's own origin with
  `credentials: "omit"`. The exchange, action, job and artifact requests also
  use `cache: "no-store"` and `redirect: "error"`. Only action, job and job
  artifact requests carry `X-Vosu-Tool-Session`; `settings()`, subscriptions,
  resources, trees, art and every request the tool makes itself carry no
  token. The bridge never takes a URL from its caller or a job document and
  never writes cookies, `localStorage`, `sessionStorage` or IndexedDB.

## State through API v1

A tool reads game state exactly as an overlay does: public API v1 reads
under the overlay listener's peer policy, no token
([API v1](../v2/api-v1.md)). `requires`/`optional` `field:` and `resource:`
entries declare what it reads, for the install plan and provider resolution;
they are not a read firewall.

**Pointers.** `fields` are RFC 6901 JSON Pointers into one session's merged
document, keyed by namespace, then by the namespace's own fields
([state schema](../v2/state-schema.md)):

| Pointer | Value |
| --- | --- |
| `/vosu.client/name`, `/vosu.client/game` | The attached client's identity |
| `/vosu.status/status` | `menu`, `selection`, `play`, `pause`, `results` or `null` |
| `/vosu.chart/title`, `/vosu.chart/artist`, `/vosu.chart/difficultyName` | Chart metadata |
| `/vosu.chart/resources/background` | A resource reference (below) |
| `/vosu.play/combo`, `/vosu.play/accuracy`, `/vosu.play/judgements` | Play values; `accuracy` is 0–1 under `accuracyKind` |
| `/vosu.skin/tree` | A tree reference to the current skin's files |
| `/vosu.osu/…` | The osu! plugin's namespace; `/vosu.osu/internal` is never present |
| `""` | The whole document |

A request holds 1–256 non-overlapping pointers. A pointer to a field or
namespace that is not filled is valid: it is simply absent, never an error and
never a zero. A namespace the session's game does not cover is absent
entirely; a field the game does not fill is `null`.

**Frames.** Each frame addresses pointers by their index in `fields`:

- `[0, revision, [[index, value], …]]` — **initial**: every present value.
  Discard everything held before it. It arrives on every (re)connect and
  whenever `active` moves to another session.
- `[1, revision, [[index, value], …], [removedIndex, …]]` — **update**:
  changed values, then indices that became absent. Delete removed indices;
  never keep a stale value.
- `[2, code, message]` — **error**, then the server closes the socket.

**Active and fixed sessions.** The default `session: "active"` follows the
focused game: a focus change sends a fresh initial frame on the same socket,
and losing all focus reports every value absent. While no game is focused, the
upgrade itself is refused (404), so `onStatus` reports `closed` and the bridge
keeps retrying with backoff until one is. A fixed session id
(`{session: "<id>"}`, from `GET /api/v1/sessions`) never follows another
session: when it ends, a final update removes every present index and the
socket closes. The bridge would keep retrying a session that no longer exists,
so call the stop function once that session is gone; follow
`WS /api/v1/sessions/subscribe` or `GET /api/v1/sessions` to learn which
sessions exist.

**Resources.** State never carries paths. A file is a reference
`{"url": "/api/v1/resources/<id>", "type": "<mime>"}`; a folder is
`{"url": "/api/v1/trees/<id>/"}`, whose `GET` lists
`{"entries":[{"path","size","type","hash"?}],"truncated","revision"}` and
whose `<url><path>` serves one member. Use the `url` as given (`<img src>`,
`fetch`, `<audio>`), with no token; a reference that has rotated out answers
410, so re-read the state for its replacement. One-shot HTTP reads
(`GET /api/v1/sessions/{id|active}/state[/<namespace>][?pointer=…]`) create no
subscription demand.

## Actions

An action is plugin code a tool may ask to run. Its capability id is
`action:<plugin-id>.<name>`; the bridge, routes and jobs use the bare
`<plugin-id>.<name>` (for example `vosu.osu.beatmap.import`). A tool may
invoke an action only when:

- the action's capability is in its `requires` or `optional`;
- that capability resolves to an installed, enabled provider;
- the request carries this tab's live launch session.

`vosu.canWrite` says the page holds a session; core still validates the
token and grant at request time. The bridge refuses to send a request
without its session (`forbidden`); the API returns 401 `unauthorized` for a
missing or stale token and 403 `forbidden` if the action is not granted or
no longer resolves. Core rechecks live launch authority and the action
grant on job requests and also checks job ownership.

Each action declares a **session mode** in its plugin's manifest, which
decides the valid `session` option:

| Mode | `session` option | Route |
| --- | --- | --- |
| `required` | omitted (`"active"`) or a session id | `/api/v1/sessions/{session}/actions/{action}` |
| `none` | `null` | `/api/v1/actions/{action}` |
| `optional` | either | either |

Calling an action at the route its mode does not allow is 404
`unknown_action`. `active` is resolved once, when the job is created; a job
never moves to a newly focused session. Arguments are validated against the
action's schema (400 `invalid_arguments`); failures, `timed_out` and
`plugin_crashed` arrive as the job's error code. Cancellation is advisory: a
result that completes first stands. A tab can read and cancel only the jobs it
created.

The core deadline is 30 seconds, separate from the HTTP `wait` parameter.
`vosu.start` creates an ordinary job with the same deadline as `vosu.invoke`.
For longer work, use short actions around a
[plugin-owned operation](../v2/api-v1.md#plugin-owned-operations-af3): the
plugin returns its own `operationId`, not a job id. When a render or export
is ready, a short output action can declare an
[artifact](../v2/api-v1.md#job-artifacts-af12) on its own job. Use
`handle.artifacts()` on the job from `vosu.start`; `vosu.invoke` returns only
the action result.

The native CLI uses a different credential. It can invoke only a provider
action marked `cli = true`, via `vosux <plugin-id> <action> [json-args]` or
`vosu x <plugin-id> <action> [json-args]`. It does not run a tool CLI or
inherit a browser launch session. See [CLI usage](../../README.md#tools).

## Catalog sources and installation

vosu 2.0 installs catalog packages through catalog v2 install plans:
sources, write-only credentials, source trust, the private-network
exception, plan review and confirmation, catalog jobs, uninstall and catalog
art are the dashboard-only `/api/catalog/*` routes in
[catalog v2 "API placement"](../v2/catalog-v2.md#api-placement), driven by
**Settings / Catalog sources**, the Tools page and the Available page. Every
`POST`/`DELETE` there requires dashboard provenance and `X-Vosu-CSRF`; API
keys, the native broker credential and overlay origins never substitute.

A publisher lists a tool in a [`catalog-v2.json`](../v2/catalog-v2.md#catalog-v2json)
`tools` entry that mirrors its manifest (`id`, `name`, `version`,
`description`, `author`, `homepage`, `icon_url`, `banner_url`, `host`,
`requires`, `optional`, `replaces`) and names one `any` package: an HTTPS
`url` without credentials, the ZIP's SHA-256 and its exact byte `size`. The
ZIP has `manifest.toml` at its root, not inside an enclosing directory. Check
a catalog and its extracted packages with the SDK's
[`vosu-package-check`](../v2/catalog-v2.md#package-checker-af10) before
publishing; see [Create a tool](create-a-tool.md#9-package-and-publish).

During [1.x coexistence](../v2/catalog-v2.md#1x-coexistence), official
releases also carry the frozen `catalog.json` that still points to the old
1.x ZIPs. It is not another index for these 2.0 packages.

Catalog art (`icon_url`, `banner_url`) loads through `GET /api/catalog/art`,
inert and sandboxed; installed tools' art keeps coming from
`/tools/<id>/_icon` and `/_banner`.

## From 1.x

See [Migrating from 1.x to 2.0](../migration/2.0.md#port-a-service-or-overlay-package)
for the service and metadata port, replacements and the immutable 1.x
guide. The [field mapping](../v2/state-schema.md#1x-to-20-field-mapping)
maps legacy state to API v1.
