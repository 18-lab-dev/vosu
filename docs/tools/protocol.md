---
type: Contract
title: "Tool protocol v1"
description: "Manifest, process, HTTP, browser-bridge, and dashboard contract between vosu and hosted tools."
---

# Tool protocol v1

A **tool** is an application that vosu discovers, supervises, and serves next to overlays. Tools may ship a browser UI, a background HTTP service, or both. Any language that can read environment variables, write one stdout line, and serve HTTP on loopback can implement a tool. Rust tools may use the `vosu-tool-sdk` crate. The decision record is [ADR 0017](../decisions/0017-tool-host.md).

## Layout and discovery

Each tool is one directory containing `tool.toml`. vosu scans two roots, in order:

| Root | Location | Trust |
| --- | --- | --- |
| Bundled | `<directory of the vosu executable>/tools/<id>/` (Linux packages: the packaged `tools` directory next to the executable) | Trusted; shipped and verified with vosu |
| User | `<vosu data directory>/tools/<id>/` | Untrusted until the user approves it in the dashboard |

A user tool whose `id` equals a bundled tool's is reported as `invalid` (duplicate). The directory name must equal `id`.

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
command = ["bin/vosu-skill-tracker"]  # argv; argv[0] relative to the tool directory.
                                 # On Windows ".exe" is appended when argv[0] has no extension.
startup = "always"               # "always" (start with vosu) | "on-demand" (first request; stopped after 10 idle minutes)

[api]                            # routes reachable through /tools/<id>/api/; anything unlisted is 404
read = ["/v1/profile", "/v1/history", "/v1/current", "/v1/stream"]  # path prefixes; GET/HEAD and WebSocket upgrade; open to overlays
write = ["/v1/import"]           # path prefixes; every method; requires a UI session or a dependent tool

[[requires]]                     # optional, repeated: tools this tool calls
id = "difficulty"
version = ">=0.1.0"              # semver requirement on the dependency's manifest version

[permissions]                    # optional; shown at approval, enforced for vosu host APIs
host = ["context", "songs:write"]  # see "Host API" below
```

`settings.json` in the tool directory, when present, uses exactly the overlay `settings.json` schema and is rendered by the dashboard's existing settings editor. Profiles do not apply to tools.

Prefix matching is segment-aware: `/v1/beatmap` matches `/v1/beatmap` and `/v1/beatmap/…`, never `/v1/beatmaps`. `write` wins when a path matches both lists.

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

- `read` routes: GET/HEAD/WebSocket from anyone permitted by the overlay listener's peer policy; CORS `*` like other overlay read APIs.
- `write` routes: require `X-Vosu-Tool-Session` (a UI session for this tool) or `X-Vosu-Tool-Token` of a running tool that lists this tool in `[[requires]]`. Otherwise 403.
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

Launch: the dashboard's **Open** calls `POST /api/tools/<id>/launch` (dashboard origin, CSRF) and opens the returned URL `<overlay origin>/tools/<id>/#vosu-launch=<code>&theme=<dark|light>`. The code is single-use and expires after 60 seconds; the bridge exchanges it via `POST /tools/_host/v1/session` `{"tool","code"}` → `{"session"}`, stores it in `sessionStorage`, and strips the fragment. Sessions last until vosu restarts. A UI opened without a launch code is read-only.

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
  "source": "bundled", "trusted": true, "enabled": true,
  "kind": "hybrid", "startup": "always",
  "status": "running", "status_detail": null,
  "ui_url": "http://127.0.0.1:24050/tools/skill-tracker/",
  "requires": [{"id": "difficulty", "version": ">=0.1.0", "satisfied": true}],
  "required_by": [],
  "permissions": {"host": ["context"]},
  "api": {"read": ["/v1/profile"], "write": ["/v1/import"]},
  "has_settings": true,
  "data": {"data_dir": "…", "cache_dir": "…", "data_bytes": 12345, "cache_bytes": 0},
  "process": {"pid": 1234, "started_at": "2026-10-21T04:00:00Z", "restarts": 0}
}
```

`status` is one of `stopped`, `starting`, `running`, `failed`, `blocked` (a dependency is missing, disabled, or unsatisfied), `untrusted`, `disabled`, `invalid` (manifest error; `status_detail` explains). UI-only tools report `running` when enabled.

Starting a tool first starts its service dependencies. Dependency cycles make every tool in the cycle `invalid`.
