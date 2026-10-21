---
type: Contract
title: "Tool protocol v1"
description: "Manifest, process, HTTP, browser-bridge, and dashboard contract between vosu and hosted tools."
---

# Tool protocol v1

A **tool** is an application that vosu discovers, supervises, and serves next to overlays. Tools may ship a browser UI, a background HTTP service, or both. Any language that can read environment variables, write one stdout line, and serve HTTP on loopback can implement a tool. Rust tools may use the `vosu-tool-sdk` crate; [`examples/tools/hello-python`](../../examples/tools/hello-python/) is a dependency-free Python reference. The decision record is [ADR 0017](../decisions/0017-tool-host.md). Editors can validate manifests with [`tool.schema.json`](tool.schema.json).

## Layout and discovery

Each tool is one directory containing `tool.toml`. vosu scans these roots, in order:

| Root | Location | Trust |
| --- | --- | --- |
| Development | Each `--tool <directory>` passed on the vosu command line | Trusted for that run: the user launched it |
| Bundled | Release-build embedded tree, verified and materialized under `<vosu cache directory>/tools/bundled/bundled-<version>-<tree digest>/` | Trusted; shipped inside the verified vosu executable |
| User | `<vosu data directory>/tools/<id>/` | Untrusted until the user approves it in the dashboard |

A development tool replaces a bundled or user tool with the same `id` for that run, so a checkout of a first-party tool can be developed against an installed vosu. A user tool that duplicates a bundled `id` is reported as `invalid`. In bundled and user roots the directory name must equal `id`.

vosu watches every root. Added, removed, or edited manifests and `settings.json` files apply without restarting vosu. For development tools, a change to the service executable also restarts a running service, so `cargo build` or saving a script is the whole edit loop.

Release builds set `VOSU_BUNDLED_TOOLS` to an absolute staged directory whose immediate children are tool ids. Normal development builds embed no tools. Materialization uses a temporary sibling, synced files and atomic rename, checks cached bytes before reuse, and removes obsolete bundled versions. Release archives retain the updater's single-root-executable invariant.

For a development service, a missing relative command is resolved by file name in Cargo output: starting at the tool directory, each ancestor containing `Cargo.toml` is checked for `$CARGO_TARGET_DIR/{debug,release}/<file>` (a relative environment value is resolved against that ancestor), or `target/{debug,release}/<file>` when the variable is unset. Debug is tried before release; the nearest matching ancestor wins. Bare executable names such as `python3` resolve through `PATH`; paths containing a separator resolve under the tool directory before the development fallback. On Windows an extensionless executable gets `.exe`.

## Manifest (`tool.toml`)

```toml
manifest_version = 1             # required; vosu 1.2.0 accepts only 1
id = "skill-tracker"             # required; ^[a-z0-9][a-z0-9-]{0,31}$, not starting with "_"
name = "Skill Tracker"           # required, display name
version = "0.1.0"                # required, semver
description = "…"                # optional, one paragraph
author = "18-LAB"                # optional
homepage = "https://…"           # optional, http(s)
icon = "web/icon.svg"            # optional, path inside the tool directory (svg/png)

[ui]                             # optional: browser UI
root = "web"                     # static directory, served at /tools/<id>/
entry = "index.html"             # default "index.html"

[service]                        # optional: background process
command = ["bin/vosu-skill-tracker"]  # argv; relative paths containing a separator resolve inside the tool directory.
                                 # Bare names (e.g. "python") resolve through PATH. On Windows extensionless executable paths gain ".exe".
startup = "always"               # "always" (start with vosu) | "on-demand" (default; first request, stop after 10 idle minutes)

[cli]                            # optional: terminal process; same command resolution as service
command = ["bin/vosu-skill-tracker", "cli"]
summary = "Profile and play history"  # optional one-line description

[api]                            # routes reachable through /tools/<id>/api/; anything unlisted is 404
read = ["/v1/profile", "/v1/history", "/v1/current", "/v1/stream"]  # path prefixes; GET/HEAD and WebSocket upgrade; open to overlays
write = ["/v1/import"]           # authenticated mutations, plus authenticated GET/HEAD where not also in read

[[requires]]                     # optional, repeated: tools this tool calls
id = "difficulty"
version = ">=0.1.0"              # semver requirement on the dependency's manifest version

[permissions]                    # optional; shown at approval, enforced for vosu host APIs
host = ["context", "songs:write"]  # see "Host API" below
```

`settings.json` in the tool directory, when present, uses exactly the overlay `settings.json` schema and is rendered by the dashboard's existing settings editor. Profiles do not apply to tools.
Editors supporting JSON Schema for TOML (Taplo / Even Better TOML) can put `#:schema https://raw.githubusercontent.com/shiruviaa/vosu/main/docs/tools/tool.schema.json` on the first line. A local manifest can instead use `#:schema ../../docs/tools/tool.schema.json`, adjusted relative to that manifest. The schema validates structure; vosu additionally checks filesystem confinement, dependencies, and semantic versions.


Prefix matching is segment-aware: `/v1/beatmap` matches `/v1/beatmap` and `/v1/beatmap/…`, never `/v1/beatmaps`. Access is method-aware: GET/HEAD/WebSocket upgrades matching `read` are open even when the same prefix is also in `write`. Other methods require a matching `write` prefix and authenticated write authority. GET/HEAD on write-only prefixes also require that authority; unlisted routes return 404.

## Process contract

vosu starts a service with the tool directory as working directory, stdin piped, stdout/stderr captured, and this environment (all other inherited variables are kept):

| Variable | Meaning |
| --- | --- |
| `VOSU_TOOL_PROTOCOL` | `1` |
| `VOSU_TOOL_ID` | The tool `id` |
| `VOSU_TOOL_TOKEN` | Per-launch random secret (64 hex characters). Never log it or send it to a browser. |
| `VOSU_URL` | Base URL of the vosu overlay listener reachable from this machine, e.g. `http://127.0.0.1:24050`, no trailing slash |
| `VOSU_TOOL_DATA_DIR` | Durable per-tool directory (created). Kept when the tool is disabled. |
| `VOSU_TOOL_CACHE_DIR` | Evictable per-tool directory (created). The dashboard may clear it while the tool is stopped. |
| `VOSU_TOOL_INTERFACE` | `service` for supervised HTTP services; `cli` for a command-line invocation |

Readiness: the service binds a TCP listener on `127.0.0.1` port `0`, then writes exactly one line `vosu-tool-ready <port>` to stdout. vosu waits up to 20 seconds; afterwards it probes `GET /healthz` (with the token header) and expects 2xx. Other stdout/stderr lines go to a bounded per-tool log shown in the dashboard.

Shutdown: vosu closes the service's stdin; the service must exit within 5 seconds, after which vosu kills it. A service that exits unexpectedly is restarted with backoff (1, 2, 4, 8, 16 s); five crashes within five minutes mark it `failed` until a manual start. Services must also exit when stdin reaches EOF, so they never outlive vosu.

Every request vosu forwards carries `X-Vosu-Tool-Token: <VOSU_TOOL_TOKEN>`. Services must reject (401) requests without the exact token: other local programs can reach the loopback port. vosu also sets `X-Vosu-Caller` to `overlay`, `ui`, or `tool:<id>` (informational).

## HTTP surface on the overlay listener

The `/tools` path prefix is reserved on the overlay listener: overlays cannot be installed, served, or configured under `tools`.

| Route | Behavior |
| --- | --- |
| `GET /tools` | 302 to the dashboard Tools page |
| `/tools/<id>/api/<rest>` | Proxied to the service as `/<rest>` with query string, method, body (≤ 1 MiB), and WebSocket upgrades on `read` routes. Hop-by-hop headers, cookies, and `X-Vosu-*` request headers are stripped and re-set by vosu. On-demand services are started and the request held until ready. 503 when the tool is disabled, untrusted, failed, or blocked. |
| `/tools/<id>/<path>` | Static file from `[ui].root` (root-confined, traversal-safe); `/tools/<id>/` serves `entry`. 404 for service-only tools. |
| `/tools/_host/v1/*` | Host API and browser bridge (below) |

Access rules for `/tools/<id>/api/*`:

- `read` routes: GET/HEAD/WebSocket from anyone permitted by the overlay listener's peer policy; CORS `*` like other overlay read APIs. Matching a `write` prefix does not change the open-read rule.
- Mutating methods on `write` routes, and GET/HEAD on write-only routes: require `X-Vosu-Tool-Session` (a UI session for this tool) or `X-Vosu-Tool-Token` of a running tool that lists this tool in `[[requires]]`. Otherwise 403. Methods other than GET/HEAD never use `read` alone; WebSocket upgrades require a read prefix.
- A dependent tool presenting its token gets both `read` and `write` routes of its declared dependencies.

## Host API (for services)

Base `VOSU_URL/tools/_host/v1`, authenticated with `X-Vosu-Tool-Token`. Public overlay APIs (`/json/v2`, `/websocket/v2/subscribe`, `/files/beatmap/*`, `/api/calculate/pp`) need no token.

| Route | Permission | Response |
| --- | --- | --- |
| `GET /self` | none | `{"id","version","data_dir","cache_dir"}` |
| `GET /settings` | none | Effective settings object `{ "<setting id>": value }` with defaults applied |
| `GET /context` | `context` | `{"client": "stable"\|"lazer"\|null, "songs_dir": abs path\|null, "beatmap": null \| {"checksum","mode","folder","file","audio","background"}}`; `checksum` is the .osu MD5, `mode` is `"osu"\|"taiko"\|"fruits"\|"mania"` (the played ruleset, including converts), paths are absolute filesystem paths of the selected instance (`audio`/`background` may be null). |

`songs:write` is advisory: it declares that the service writes into the osu! songs directory and is shown at approval. Host permissions limit what vosu APIs return; they do not sandbox the process, which runs with the user's OS rights. Approving a user tool is an explicit trust decision.

## Browser bridge (for UIs)

UIs load `<script type="module">` from `/tools/_host/v1/bridge.js` and may link `/tools/_host/v1/theme.css` (the dashboard design tokens; `:root[data-theme="dark"|"light"]`).

```js
import { vosu } from "/tools/_host/v1/bridge.js";
await vosu.ready;            // exchanges a launch code from the URL fragment, applies data-theme
vosu.tool;                   // { id, name, version }
vosu.canWrite;               // true when this tab holds a UI session
vosu.dashboardUrl;           // dashboard origin, for "Settings" links
await vosu.fetch("v1/profile");          // relative to /tools/<id>/api/, adds the session header
vosu.subscribe(fields, onFrame);         // /websocket/v2/subscribe helper
```

Launch: the dashboard's **Open** calls `POST /api/tools/<id>/launch` (dashboard origin, CSRF) and opens the returned URL `<overlay origin>/tools/<id>/#vosu-launch=<code>&theme=<dark|light>` in a new tab without an opener. The code is single-use and expires after 60 seconds; the bridge exchanges it via `POST /tools/_host/v1/session` `{"tool","code"}` → `{"session"}`, keeps the session only in the page's JavaScript memory, and strips the fragment. Never persist it to `sessionStorage`, `localStorage`, IndexedDB, or cookies: those are shared with every overlay that later loads in the same tab. Sessions last until vosu restarts. A UI opened without a launch code, or reloaded, is read-only until opened from the dashboard again. Tool UIs must not open same-origin pages without `noopener` or embed overlays in frames, because such documents can script the tool page and use its session.

## Dashboard API (dashboard listener only)

All POSTs require dashboard provenance and `X-Vosu-CSRF`, like other management mutations.

| Route | Purpose |
| --- | --- |
| `GET /tools` | Tools page |
| `GET /api/tools` | `{"tools": [ToolView]}` |
| `GET /api/tools/<id>` | `ToolView` |
| `GET /api/tools/<id>/logs?after=<seq>` | `{"lines": [{"seq","ts","stream": "stdout"\|"stderr"\|"host","text"}]}` (bounded, newest 1000) |
| `POST /api/tools/<id>/{enable,disable,start,stop,restart,trust,launch,open-data,clear-cache}` | Lifecycle and data actions; `launch` returns `{"url"}` |
| `GET`/`POST /api/tools/<id>/settings` | Same request/response shapes as `/api/counters/settings/<name>` without profiles |

`ToolView`:

```json
{
  "id": "skill-tracker", "name": "Skill Tracker", "version": "0.1.0",
  "description": "…", "author": "18-LAB", "homepage": null, "icon_url": "http://127.0.0.1:24050/tools/skill-tracker/icon.svg",
  "source": "bundled", "path": "…/tools/skill-tracker", "trusted": true, "enabled": true,
  "kind": "hybrid", "startup": "always",
  "status": "running", "status_detail": null,
  "ui_url": "http://127.0.0.1:24050/tools/skill-tracker/",
  "cli": {"usage": "vosux skill-tracker", "summary": "Profile and play history"},
  "requires": [{"id": "difficulty", "version": ">=0.1.0", "satisfied": true}],
  "required_by": [],
  "permissions": {"host": ["context"]},
  "api": {"read": ["/v1/profile"], "write": ["/v1/import"]},
  "has_settings": true,
  "data": {"data_dir": "…", "cache_dir": "…", "data_bytes": 12345, "cache_bytes": 0},
  "process": {"pid": 1234, "started_at": "2026-10-21T04:00:00Z", "restarts": 0}
}
```

`source` is `dev`, `bundled`, or `user`. `kind` is `ui`, `service`, `hybrid` (UI and service), or `cli` (CLI only). `status` is one of `stopped`, `starting`, `running`, `failed`, `blocked` (a dependency is missing, disabled, or unsatisfied), `untrusted`, `disabled`, `invalid` (manifest error; `status_detail` explains). Tools without a service report `running` when enabled; CLI processes are owned by their launchers, not supervised services.

Starting a tool first starts its service dependencies. Dependency cycles make every tool in the cycle `invalid`.

## Conventions for tool APIs

These are not enforced, but first-party tools follow them so overlays and other tools can rely on the same shapes:

- Version every route under `/v1/`; add `/v2/` beside it rather than changing `/v1/` shapes.
- Errors are JSON `{"error": "<message>"}` with a 4xx/5xx status.
- Work longer than about a second is a job: `POST` returns `202 {"job": "<id>"}`, `GET /v1/jobs/<id>` returns `{"state": "queued"|"running"|"done"|"failed"|"cancelled", "progress": 0.0..1.0, "message", "result"}`, and `DELETE /v1/jobs/<id>` cancels.
- Live data is a WebSocket on a `read` route that sends a full JSON snapshot on connect and on every change.

## Command-line tools (vosux)

`vosu x [OPTIONS] <tool> [ARGS...]` and `vosux [OPTIONS] <tool> [ARGS...]`
use the same executable. `vosux` is argv[0] dispatch, case-insensitive on Windows
with `.exe` stripped, not another binary. Linux distro packages install a sibling
`vosux -> vosu` symlink; Windows ZIPs stay single-executable. The launcher performs
no daemon initialization, logging, listener, tray, updater, or single-instance work.

Options are recognized only before the tool id: `--list` (also the default with
no tool), `--list --json`, `--help`, and `--open <tool>`. Every argument after the
id, including `--help`, `--`, whitespace, and option-like text, goes verbatim to
the child without a shell. A tool with `[ui]` but no `[cli]` opens its UI when
invoked without arguments; arguments instead produce usage error 2.

`[cli].command` is a nonempty argv array, resolved exactly as `[service].command`,
including development Cargo fallback. `summary` is optional and cannot contain
NUL or line breaks. CLI-only tools are allowed. ToolView always includes `cli`:
`null` or `{"usage":"vosux <id>","summary":"…"}` (summary may be null).
The dashboard shows that usage and summary.

The launcher discovers `origin` and `token` through the per-user
[`broker.json`](../../README.md#native-overlay-brokers). The following overlay
routes require a transport-observed loopback peer and
`Authorization: Bearer <native broker token>`, even for reads:

| Route | Response |
| --- | --- |
| `GET /tools/_host/v1/cli` | `{"tools":[{"id","name","version","status","cli","ui":true,"trusted":true,"enabled":true}]}` |
| `POST /tools/_host/v1/cli/<id>` | `{"session":"opaque id","argv":["absolute executable","…"],"cwd":"tool directory","env":{…}}` |
| `DELETE /tools/_host/v1/cli/session/<session>` | Revokes the session (idempotent) |
| `POST /tools/_host/v1/cli/<id>/open` | `{"url":"…"}` with the same single-use launch code as dashboard Open; optional JSON `{"theme":"light"}` or `"dark"`, default dark |

Unknown tools return 404; no CLI returns 409; untrusted, disabled, invalid or
dependency-blocked tools return 403 with `error` and `status`. Session exhaustion
returns 503. Each session has a fresh 256-bit token; at most 64 live sessions
coexist and expire after 24 hours or daemon restart. Disable/replacement and
explicit revocation invalidate tokens. Expired/revoked tokens fail host API
authentication (401) and proxy authority checks (403).

The returned environment contains `VOSU_TOOL_PROTOCOL`, `VOSU_TOOL_ID`,
`VOSU_TOOL_TOKEN`, `VOSU_URL`, `VOSU_TOOL_DATA_DIR`, `VOSU_TOOL_CACHE_DIR`, and
`VOSU_TOOL_INTERFACE=cli`. Merge it over the launcher's current environment.
CLI tokens have their tool's permission-filtered host API access, read/write
access to declared dependencies, and read/write access to their own declared
`/tools/<id>/api/` routes. No access to undeclared tools is granted. The proxy
sets `X-Vosu-Caller: cli:<id>` and replaces the token with the target service
credential, retaining the service as the single writer of shared state.

The child runs directly in the returned cwd with inherited stdin/stdout/stderr.
After child exit (or spawn failure), the launcher best-effort revokes the session.
Exit status is the child's; Unix signal exits map to `128 + signal`.
Launcher errors: 2 usage, 69 daemon unavailable (missing/stale broker file or
connection refused), 77 tool not runnable, 127 unknown tool, 1 other.
Unix children stay in the foreground process group: terminal Ctrl+C/Ctrl+\\
reach the child directly while vosux waits; SIGTERM/SIGHUP are forwarded to the
child. Windows children share the console; a temporary vosux console handler
consumes Ctrl+C/Ctrl+Break in the launcher only, letting the child decide.

